//! VFIO-backed hardware access.
//!
//! Step 2 of 7: bind dance + container/group open + BAR1 mmap.
//! Step 3 will add DMA mapping. Step 6 will add the SIGINT handler.
//!
//! References:
//!   - kernel headers: /usr/include/linux/vfio.h
//!   - kernel docs:    Documentation/driver-api/vfio.rst
//!   - prior art:      dpdk-devbind.py (bind-dance pattern), virsh nodedev-detach
//!
//! Lifecycle:
//!     let mut be = VfioBackend::open("0000:03:00.0")?;
//!     // ... use be.bar1(), be.alloc_dma() ...
//!     drop(be); // restores mpt3sas (or whatever driver was bound originally)

use super::{DmaBuffer, HwBackend, HwError};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;

// VFIO ABI — per /usr/include/linux/vfio.h.
//
// VFIO_TYPE is ';' (0x3B), VFIO_BASE is 100. The kernel _IO(type, nr) macro
// expands to `(type << 8) | nr` for the no-data-transfer ioctls used here.
// Pre-computed at compile time since these never change.
//
// Note: libc::ioctl's request type differs per libc — glibc takes c_ulong,
// musl takes c_int. Alias keeps the constants and wrappers portable so the
// musl static-binary CI build doesn't fail.
#[cfg(target_env = "musl")]
type IoctlReq = libc::c_int;
#[cfg(not(target_env = "musl"))]
type IoctlReq = libc::c_ulong;

const VFIO_GET_API_VERSION: IoctlReq = 0x3B64;
#[allow(dead_code)]
const VFIO_CHECK_EXTENSION: IoctlReq = 0x3B65;
const VFIO_SET_IOMMU: IoctlReq = 0x3B66;
const VFIO_GROUP_GET_STATUS: IoctlReq = 0x3B67;
const VFIO_GROUP_SET_CONTAINER: IoctlReq = 0x3B68;
const VFIO_GROUP_GET_DEVICE_FD: IoctlReq = 0x3B6A;
const VFIO_DEVICE_GET_REGION_INFO: IoctlReq = 0x3B6C;
const VFIO_IOMMU_MAP_DMA: IoctlReq = 0x3B71;
const VFIO_IOMMU_UNMAP_DMA: IoctlReq = 0x3B72;

const VFIO_API_VERSION: i32 = 0;
const VFIO_TYPE1_IOMMU: i32 = 1;
const VFIO_NOIOMMU_IOMMU: i32 = 8;
const VFIO_GROUP_FLAGS_VIABLE: u32 = 1 << 0;
const VFIO_DMA_MAP_FLAG_READ: u32 = 1 << 0;
const VFIO_DMA_MAP_FLAG_WRITE: u32 = 1 << 1;

/// Starting IOVA for our DMA allocations. Picked well above conventional
/// PCI MMIO ranges (4 GB - 256 GB) so we don't collide with anything the
/// kernel or firmware might reserve. Bumped per allocation.
const IOVA_BASE: u64 = 0x10_0000_0000;

/// Page size for DMA alignment. Use a hugepage size (2 MB) so a single
/// MAP_DMA covers most FW_UPLOAD buffers in one go. The kernel will pin
/// these pages for the lifetime of the mapping.
const DMA_PAGE_SIZE: usize = 2 * 1024 * 1024;

/// MAP_HUGETLB flag for mmap. libc only exposes it on Linux, so we declare
/// our own constant. Value per `bits/mman-linux.h`: 0x40000.
const MAP_HUGETLB: libc::c_int = 0x40000;

/// PCI BAR index — BAR1 is what holds the MPI doorbell registers on SAS2008.
const VFIO_PCI_BAR1_REGION_INDEX: u32 = 1;
/// PCI config-space region index per kernel vfio-pci. Used to enable bus
/// master (BME) after bind — vfio-pci leaves BME=0 for security; the
/// chip can't DMA until we set it.
const VFIO_PCI_CONFIG_REGION_INDEX: u32 = 7;
/// Offset of PCI Command register within config space (2 bytes, LE).
const PCI_COMMAND_OFFSET: u64 = 0x04;
/// Bus Master Enable bit in PCI Command register.
const PCI_COMMAND_MASTER: u16 = 0x0004;

/// VFIO group status reply.
#[repr(C)]
#[derive(Default, Debug)]
struct VfioGroupStatus {
    argsz: u32,
    flags: u32,
}

/// VFIO device region info — describes one BAR's mmap offset + size.
#[repr(C)]
#[derive(Default, Debug)]
struct VfioRegionInfo {
    argsz: u32,
    flags: u32,
    index: u32,
    cap_offset: u32,
    size: u64,
    offset: u64,
}

/// VFIO_IOMMU_MAP_DMA arg — pin pages and install IOVA->VA translation
/// in the container's IOMMU domain.
#[repr(C)]
#[derive(Default, Debug)]
struct VfioDmaMap {
    argsz: u32,
    flags: u32,
    vaddr: u64,
    iova: u64,
    size: u64,
}

/// VFIO_IOMMU_UNMAP_DMA arg — release IOMMU mapping + unpin pages.
#[repr(C)]
#[derive(Default, Debug)]
struct VfioDmaUnmap {
    argsz: u32,
    flags: u32,
    iova: u64,
    size: u64,
}

/// Tracks one outstanding DMA allocation so Drop can free everything
/// even if the caller forgot to call `free_dma`.
struct DmaAlloc {
    va: *mut u8,
    iova: u64,
    len: usize,
}

/// `VfioBackend` — owns the bind state + open VFIO fds + BAR1 mmap. Drop
/// closes everything and rebinds the original driver.
pub struct VfioBackend {
    bdf: String,
    /// Held for its Drop side-effect (close fd) — kernel keeps container alive
    /// as long as any fd to it is open.
    #[allow(dead_code)]
    container_fd: File,
    /// Same — drop closes it.
    #[allow(dead_code)]
    group_fd: File,
    device_fd: RawFd,
    bar1_ptr: *mut u8,
    bar1_len: usize,
    original_driver: Option<String>,
    /// True when host runs noiommu mode (iommu=off in cmdline). DMA paths
    /// diverge — TYPE1 hosts go through VFIO_IOMMU_MAP_DMA; noiommu hosts
    /// would need a hugepage+pagemap fallback (step 3b, not yet implemented).
    is_noiommu: bool,
    /// Next IOVA to hand out. Bumped by DMA_PAGE_SIZE per alloc. Only used
    /// in TYPE1 mode where IOMMU translates these to physical addresses.
    next_iova: u64,
    /// Live DMA allocations — used by Drop to free anything the caller leaked.
    dma_allocs: Vec<DmaAlloc>,
}

// Safety: VfioBackend's resources (fds + mmap) are all process-wide. Raw
// pointer isn't `Sync` — see DmaBuffer comment for the same reasoning.
unsafe impl Send for VfioBackend {}

impl VfioBackend {
    /// Hugepage + pagemap fallback for noiommu hosts (lab/dev only).
    ///
    /// On `iommu=off` systems the kernel doesn't translate IO addresses, so
    /// the SGE must carry a real physical address. We can't allocate "the
    /// physical memory at address X" from user-space — instead: mmap a
    /// hugepage, mlock to pin it, then ask `/proc/self/pagemap` what PA the
    /// kernel happened to give us. The hugepage means the whole allocation
    /// is one contiguous PA range — no scatter-gather of fragments.
    ///
    /// Requires root (pagemap PFNs are zeroed for non-CAP_SYS_ADMIN since
    /// kernel 4.0). dev-1 runs `lsi-flash` as root, so this is fine.
    fn alloc_dma_hugepage(&mut self, len: usize) -> Result<DmaBuffer, HwError> {
        let mapped_len = len.div_ceil(DMA_PAGE_SIZE) * DMA_PAGE_SIZE;
        if mapped_len > DMA_PAGE_SIZE {
            return Err(HwError::DmaAlloc(format!(
                "noiommu DMA buffers limited to one hugepage ({} bytes); \
                 requested {} would span multiple pages with non-contiguous PAs",
                DMA_PAGE_SIZE, mapped_len
            )));
        }

        // MAP_HUGETLB|MAP_ANONYMOUS — kernel uses a hugepage from the pool
        // configured at boot (dev-1 has hugepagesz=2M hugepages=16).
        let va = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | MAP_HUGETLB,
                -1,
                0,
            )
        };
        if va == libc::MAP_FAILED {
            return Err(HwError::DmaAlloc(format!(
                "mmap MAP_HUGETLB {} bytes: {} — \
                 check /sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages",
                mapped_len,
                std::io::Error::last_os_error()
            )));
        }

        // Pin the page so the kernel doesn't migrate or swap it. Hugepages
        // are already unswappable, but mlock also blocks NUMA migration.
        if unsafe { libc::mlock(va, mapped_len) } != 0 {
            let e = std::io::Error::last_os_error();
            unsafe {
                libc::munmap(va, mapped_len);
            }
            return Err(HwError::DmaAlloc(format!("mlock: {}", e)));
        }

        // Touch the page so the kernel actually backs it (lazy allocation).
        unsafe {
            std::ptr::write_bytes(va as *mut u8, 0, mapped_len);
        }

        // Recover PA via pagemap.
        let pa = match va_to_pa(va as usize) {
            Ok(p) => p,
            Err(e) => {
                unsafe {
                    libc::munlock(va as *const libc::c_void, mapped_len);
                    libc::munmap(va, mapped_len);
                }
                return Err(HwError::DmaAlloc(format!("va_to_pa: {}", e)));
            }
        };

        self.dma_allocs.push(DmaAlloc {
            va: va as *mut u8,
            iova: pa,
            len: mapped_len,
        });

        eprintln!(
            "vfio: alloc_dma_hugepage len={} va={:p} iova(pa)=0x{:x}",
            mapped_len, va, pa
        );

        Ok(DmaBuffer {
            va: va as *mut u8,
            iova: pa,
            len: mapped_len,
            handle: pa,
        })
    }

    pub fn open(bdf: &str) -> Result<Self, HwError> {
        // 1. preflight: vfio + vfio-pci modules loaded; noiommu mode enabled
        //    in /sys/module/vfio/parameters/enable_unsafe_noiommu_mode.
        Self::preflight()?;

        // 2. save current driver so we can restore it on failure or Drop.
        let original_driver = current_driver(bdf);

        // 3. driver_override + unbind from current + bind vfio-pci. From this
        //    point forward, any early return must NOT leave the card on
        //    vfio-pci with no live VfioBackend to restore it on Drop. The
        //    `bind_guard` defers `restore_driver` until we commit at the end.
        bind_to_vfio_pci(bdf, original_driver.as_deref())?;
        let mut bind_guard = BindGuard {
            bdf,
            original_driver: original_driver.clone(),
            committed: false,
        };

        // 4. open VFIO container.
        let container_fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/vfio/vfio")
            .map_err(|e| HwError::Preflight(format!("open /dev/vfio/vfio: {}", e)))?;

        // 5. verify API version.
        let api: i32 = ioctl_get(container_fd.as_raw_fd(), VFIO_GET_API_VERSION)
            .map_err(|e| HwError::Preflight(format!("VFIO_GET_API_VERSION: {}", e)))?;
        if api != VFIO_API_VERSION {
            return Err(HwError::Preflight(format!(
                "VFIO API version mismatch: kernel={}, we built for={}",
                api, VFIO_API_VERSION
            )));
        }

        // 6. inspect the device's iommu_group symlink to determine both the
        //    /dev/vfio/<...> device-node path AND whether we're in noiommu
        //    mode (per-group, not a host-wide check — vfio-pci creates either
        //    real-iommu groups or noiommu-prefixed ones based on the device's
        //    actual IOMMU exposure).
        let group_info = group_info_for(bdf)?;
        let group_fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&group_info.dev_path)
            .map_err(|e| HwError::VfioGroupUnavailable {
                bdf: bdf.to_string(),
                reason: format!("open {}: {}", group_info.dev_path.display(), e),
            })?;

        // 7. group must be viable (all devices in it bound to vfio-pci).
        let mut status = VfioGroupStatus {
            argsz: std::mem::size_of::<VfioGroupStatus>() as u32,
            flags: 0,
        };
        ioctl_ref(
            group_fd.as_raw_fd(),
            VFIO_GROUP_GET_STATUS,
            &mut status as *mut _ as *mut libc::c_void,
        )
        .map_err(|e| HwError::Preflight(format!("VFIO_GROUP_GET_STATUS: {}", e)))?;
        if status.flags & VFIO_GROUP_FLAGS_VIABLE == 0 {
            return Err(HwError::VfioGroupUnavailable {
                bdf: bdf.to_string(),
                reason: "group not viable (other devices in same IOMMU group still bound to non-vfio drivers)".into(),
            });
        }

        // 8. attach group to container.
        let container_raw = container_fd.as_raw_fd();
        ioctl_ref(
            group_fd.as_raw_fd(),
            VFIO_GROUP_SET_CONTAINER,
            &container_raw as *const _ as *mut libc::c_void,
        )
        .map_err(|e| HwError::Preflight(format!("VFIO_GROUP_SET_CONTAINER: {}", e)))?;

        // 9. set IOMMU type on the container. noiommu-mode container accepts
        //    only VFIO_NOIOMMU_IOMMU; real-IOMMU container accepts VFIO_TYPE1.
        let iommu_type: IoctlReq = if group_info.is_noiommu {
            VFIO_NOIOMMU_IOMMU as IoctlReq
        } else {
            VFIO_TYPE1_IOMMU as IoctlReq
        };
        ioctl_value(container_fd.as_raw_fd(), VFIO_SET_IOMMU, iommu_type)
            .map_err(|e| HwError::Preflight(format!("VFIO_SET_IOMMU: {}", e)))?;

        // 10. get device fd via group.
        let bdf_cstr = std::ffi::CString::new(bdf).unwrap();
        let device_fd = unsafe {
            libc::ioctl(
                group_fd.as_raw_fd(),
                VFIO_GROUP_GET_DEVICE_FD,
                bdf_cstr.as_ptr(),
            )
        };
        if device_fd < 0 {
            return Err(HwError::Preflight(format!(
                "VFIO_GROUP_GET_DEVICE_FD: {}",
                std::io::Error::last_os_error()
            )));
        }

        // 11. ask the device for BAR1 region info (offset within device_fd
        //     + size). Then mmap.
        let mut region = VfioRegionInfo {
            argsz: std::mem::size_of::<VfioRegionInfo>() as u32,
            index: VFIO_PCI_BAR1_REGION_INDEX,
            ..Default::default()
        };
        ioctl_ref(
            device_fd,
            VFIO_DEVICE_GET_REGION_INFO,
            &mut region as *mut _ as *mut libc::c_void,
        )
        .map_err(|e| HwError::Bar1Mmap(format!("VFIO_DEVICE_GET_REGION_INFO: {}", e)))?;
        let bar1_len = region.size as usize;
        if bar1_len == 0 {
            return Err(HwError::Bar1Mmap(
                "BAR1 region reported zero size — chip may not be vfio-pci capable".into(),
            ));
        }
        let bar1_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                bar1_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                device_fd,
                region.offset as i64,
            )
        };
        if bar1_ptr == libc::MAP_FAILED {
            return Err(HwError::Bar1Mmap(format!(
                "mmap BAR1: {}",
                std::io::Error::last_os_error()
            )));
        }

        // 12. enable PCI bus master. vfio-pci leaves BME=0 by default for
        //     security — without this, the chip parses our FW_UPLOAD doorbell
        //     request, attempts DMA writes, but the PCIe root complex silently
        //     drops them. Chip returns Success because from its perspective it
        //     issued the writes. We discover the missing data only when we
        //     read the supposedly-DMA'd buffer and find zeros.
        //     Caught on dev-1 first-real-DMA attempt 2026-05-28.
        enable_bus_master(device_fd)?;

        // Commit the bind guard — from here on, Self's own Drop owns the
        // restore_driver responsibility.
        bind_guard.committed = true;

        Ok(VfioBackend {
            bdf: bdf.to_string(),
            container_fd,
            group_fd,
            device_fd,
            bar1_ptr: bar1_ptr as *mut u8,
            bar1_len,
            original_driver,
            is_noiommu: group_info.is_noiommu,
            next_iova: IOVA_BASE,
            dma_allocs: Vec::new(),
        })
    }

    /// Preflight: verify vfio + vfio-pci modules are loaded; load them if not.
    /// Verify noiommu mode is enabled on iommu=off hosts.
    fn preflight() -> Result<(), HwError> {
        // vfio core module
        if !PathBuf::from("/sys/module/vfio").exists() {
            modprobe("vfio")?;
        }
        // vfio-pci. May need enable_unsafe_noiommu_mode=Y on iommu=off hosts.
        if !PathBuf::from("/sys/module/vfio_pci").exists() {
            modprobe("vfio-pci")?;
        }
        // If IOMMU off in kernel cmdline, ensure vfio's noiommu mode is
        // enabled so vfio-pci will accept devices without IOMMU domains.
        // Detection: parse /proc/cmdline for `iommu=off`, `intel_iommu=off`,
        // or `amd_iommu=off`. Each appears as a standalone token.
        if iommu_disabled_in_cmdline() {
            let path = "/sys/module/vfio/parameters/enable_unsafe_noiommu_mode";
            let cur = std::fs::read_to_string(path).unwrap_or_default();
            if cur.trim() != "Y" {
                std::fs::write(path, b"Y").map_err(|e| {
                    HwError::Preflight(format!(
                        "enable noiommu mode (write to {}): {} — kernel may have CONFIG_VFIO_NOIOMMU=n",
                        path, e
                    ))
                })?;
            }
        }
        Ok(())
    }
}

/// True when any `iommu=off` / `intel_iommu=off` / `amd_iommu=off` flag is
/// present in /proc/cmdline. Used by preflight to decide whether to enable
/// vfio's unsafe-noiommu-mode parameter. The actual per-device noiommu
/// determination happens in `group_info_for` after bind.
fn iommu_disabled_in_cmdline() -> bool {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    cmdline
        .split_whitespace()
        .any(|tok| tok == "iommu=off" || tok == "intel_iommu=off" || tok == "amd_iommu=off")
}

impl HwBackend for VfioBackend {
    fn bar1(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bar1_ptr, self.bar1_len) }
    }

    fn alloc_dma(&mut self, len: usize) -> Result<DmaBuffer, HwError> {
        if self.is_noiommu {
            return self.alloc_dma_hugepage(len);
        }

        let mapped_len = len.div_ceil(DMA_PAGE_SIZE) * DMA_PAGE_SIZE;
        let container_raw = self.container_fd.as_raw_fd();

        // Anonymous mmap — backing pages for the buffer. The kernel will pin
        // these on VFIO_IOMMU_MAP_DMA, so they cannot be paged out while the
        // mapping is live.
        let va = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if va == libc::MAP_FAILED {
            return Err(HwError::DmaAlloc(format!(
                "mmap anon {} bytes: {}",
                mapped_len,
                std::io::Error::last_os_error()
            )));
        }

        let iova = self.next_iova;
        let mut map = VfioDmaMap {
            argsz: std::mem::size_of::<VfioDmaMap>() as u32,
            flags: VFIO_DMA_MAP_FLAG_READ | VFIO_DMA_MAP_FLAG_WRITE,
            vaddr: va as u64,
            iova,
            size: mapped_len as u64,
        };
        if let Err(e) = ioctl_ref(
            container_raw,
            VFIO_IOMMU_MAP_DMA,
            &mut map as *mut _ as *mut libc::c_void,
        ) {
            unsafe {
                libc::munmap(va, mapped_len);
            }
            return Err(HwError::DmaAlloc(format!(
                "VFIO_IOMMU_MAP_DMA iova=0x{:x} size={}: {}",
                iova, mapped_len, e
            )));
        }
        self.next_iova += mapped_len as u64;
        self.dma_allocs.push(DmaAlloc {
            va: va as *mut u8,
            iova,
            len: mapped_len,
        });

        Ok(DmaBuffer {
            va: va as *mut u8,
            iova,
            len: mapped_len,
            handle: iova, // IOVA is unique per live mapping — use as opaque id.
        })
    }

    fn free_dma(&mut self, buf: DmaBuffer) -> Result<(), HwError> {
        let pos = self
            .dma_allocs
            .iter()
            .position(|a| a.iova == buf.handle)
            .ok_or_else(|| {
                HwError::DmaAlloc(format!(
                    "free_dma: handle 0x{:x} not in alloc table",
                    buf.handle
                ))
            })?;
        let alloc = self.dma_allocs.swap_remove(pos);
        if self.is_noiommu {
            // No UNMAP_DMA — kernel doesn't track these. Just munlock + munmap.
            unsafe {
                libc::munlock(alloc.va as *const libc::c_void, alloc.len);
                libc::munmap(alloc.va as *mut libc::c_void, alloc.len);
            }
        } else {
            unmap_dma_one(self.container_fd.as_raw_fd(), &alloc)?;
        }
        Ok(())
    }

    fn name(&self) -> &'static str {
        "vfio"
    }

    fn bdf(&self) -> &str {
        &self.bdf
    }
}

impl Drop for VfioBackend {
    fn drop(&mut self) {
        // Free outstanding DMA allocations the caller leaked. Order matters:
        // UNMAP_DMA before closing the container_fd, BAR1 munmap before
        // closing device_fd. Best-effort — Drop runs even on panic.
        let container_raw = self.container_fd.as_raw_fd();
        for alloc in self.dma_allocs.drain(..) {
            let _ = unmap_dma_one(container_raw, &alloc);
        }
        unsafe {
            libc::munmap(self.bar1_ptr as *mut libc::c_void, self.bar1_len);
            libc::close(self.device_fd);
        }
        // group_fd + container_fd close on their own File drops.
        // Rebind original driver.
        let _ = restore_driver(&self.bdf, self.original_driver.as_deref());
    }
}

/// UNMAP_DMA + munmap a single allocation. Factored out because both
/// `free_dma` and the Drop sweep need it.
fn unmap_dma_one(container_fd: RawFd, alloc: &DmaAlloc) -> Result<(), HwError> {
    let mut unmap = VfioDmaUnmap {
        argsz: std::mem::size_of::<VfioDmaUnmap>() as u32,
        flags: 0,
        iova: alloc.iova,
        size: alloc.len as u64,
    };
    let ioctl_err = ioctl_ref(
        container_fd,
        VFIO_IOMMU_UNMAP_DMA,
        &mut unmap as *mut _ as *mut libc::c_void,
    );
    unsafe {
        libc::munmap(alloc.va as *mut libc::c_void, alloc.len);
    }
    ioctl_err.map_err(|e| {
        HwError::DmaAlloc(format!(
            "VFIO_IOMMU_UNMAP_DMA iova=0x{:x}: {}",
            alloc.iova, e
        ))
    })?;
    Ok(())
}

// === free helpers =========================================================

fn modprobe(name: &str) -> Result<(), HwError> {
    let status = std::process::Command::new("modprobe")
        .arg(name)
        .status()
        .map_err(|e| {
            HwError::ModuleMissing(Box::leak(
                format!("modprobe {}: {}", name, e).into_boxed_str(),
            ))
        })?;
    if !status.success() {
        return Err(HwError::ModuleMissing(Box::leak(
            format!("modprobe {} failed (status {})", name, status).into_boxed_str(),
        )));
    }
    Ok(())
}

/// Per-device VFIO group info — both the /dev/vfio/<...> device-node path
/// AND whether this group is in noiommu mode. Determined by reading the
/// device's iommu_group symlink (which only exists AFTER bind to vfio-pci)
/// and checking for the kernel-created `noiommu` marker file inside the
/// group's sysfs directory.
struct VfioGroupInfo {
    dev_path: PathBuf,
    is_noiommu: bool,
}

/// Look up VFIO group info for `bdf` after bind. Returns the right device-node
/// path (`/dev/vfio/<N>` for real-IOMMU groups, `/dev/vfio/noiommu-<N>` for
/// noiommu groups) so the caller doesn't have to guess.
fn group_info_for(bdf: &str) -> Result<VfioGroupInfo, HwError> {
    let link = PathBuf::from(format!("/sys/bus/pci/devices/{}/iommu_group", bdf));
    // canonicalize follows the symlink AND any further symlinks; gives us
    // the real /sys/kernel/iommu_groups/<N> path.
    let target = std::fs::canonicalize(&link).map_err(|e| HwError::VfioGroupUnavailable {
        bdf: bdf.to_string(),
        reason: format!(
            "canonicalize {}: {} (device may not be bound to vfio-pci yet)",
            link.display(),
            e
        ),
    })?;
    let group_num = target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| HwError::VfioGroupUnavailable {
            bdf: bdf.to_string(),
            reason: format!("iommu_group target has no basename: {:?}", target),
        })?
        .to_string();
    // noiommu detection: the kernel sets <group>/name to "vfio-noiommu" when
    // vfio-pci binds a device in noiommu mode. (Verified on dev-1 Ubuntu
    // 24.04 / kernel 6.8: no marker FILE exists; the indicator is the name
    // sysfs attribute.) Real-IOMMU groups have name absent or empty.
    let name_path = target.join("name");
    let name = std::fs::read_to_string(&name_path).unwrap_or_default();
    let is_noiommu = name.trim() == "vfio-noiommu";
    let dev_path = if is_noiommu {
        PathBuf::from(format!("/dev/vfio/noiommu-{}", group_num))
    } else {
        PathBuf::from(format!("/dev/vfio/{}", group_num))
    };
    Ok(VfioGroupInfo {
        dev_path,
        is_noiommu,
    })
}

/// RAII guard: if the entire `VfioBackend::open` doesn't complete, restore the
/// original driver so the card doesn't stay bound to vfio-pci with no live
/// backend to manage it. Set `committed = true` on success — then `Self`'s
/// own Drop takes over the restore responsibility.
struct BindGuard<'a> {
    bdf: &'a str,
    original_driver: Option<String>,
    committed: bool,
}

impl Drop for BindGuard<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = restore_driver(self.bdf, self.original_driver.as_deref());
        }
    }
}

/// Read /sys/bus/pci/devices/<bdf>/driver to get the currently-bound driver.
/// Returns None if no driver is bound (orphan device).
fn current_driver(bdf: &str) -> Option<String> {
    let link = PathBuf::from(format!("/sys/bus/pci/devices/{}/driver", bdf));
    std::fs::read_link(link).ok().and_then(|target| {
        target
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    })
}

/// Bind a PCI device to vfio-pci. Uses driver_override + drivers_probe — the
/// dpdk-devbind.py pattern. Idempotent: no-op if already bound to vfio-pci.
fn bind_to_vfio_pci(bdf: &str, current: Option<&str>) -> Result<(), HwError> {
    if current == Some("vfio-pci") {
        return Ok(());
    }
    let override_path = format!("/sys/bus/pci/devices/{}/driver_override", bdf);
    std::fs::write(&override_path, b"vfio-pci\n").map_err(|e| {
        HwError::DriverBind(format!("write driver_override={}: {}", override_path, e))
    })?;
    if let Some(drv) = current {
        let unbind_path = format!("/sys/bus/pci/drivers/{}/unbind", drv);
        // Already-unbound is fine; ignore "no such device" errors specifically.
        let _ = std::fs::write(&unbind_path, format!("{}\n", bdf));
    }
    let probe_path = "/sys/bus/pci/drivers_probe";
    std::fs::write(probe_path, format!("{}\n", bdf))
        .map_err(|e| HwError::DriverBind(format!("drivers_probe {}: {}", bdf, e)))?;
    // Tiny settle delay — udev/sysfs need a beat after the bind.
    std::thread::sleep(std::time::Duration::from_millis(200));
    if current_driver(bdf).as_deref() != Some("vfio-pci") {
        return Err(HwError::DriverBind(format!(
            "expected vfio-pci after bind, got {:?}",
            current_driver(bdf)
        )));
    }
    Ok(())
}

/// Reverse `bind_to_vfio_pci` — unbind from vfio-pci, clear driver_override,
/// let the kernel reprobe the original driver (mpt3sas). Best-effort: if any
/// step fails we log and continue, since this runs in Drop.
fn restore_driver(bdf: &str, original: Option<&str>) -> Result<(), HwError> {
    let unbind_path = "/sys/bus/pci/drivers/vfio-pci/unbind";
    let _ = std::fs::write(unbind_path, format!("{}\n", bdf));
    let override_path = format!("/sys/bus/pci/devices/{}/driver_override", bdf);
    let _ = std::fs::write(&override_path, b"\n");
    let _ = std::fs::write("/sys/bus/pci/drivers_probe", format!("{}\n", bdf));
    std::thread::sleep(std::time::Duration::from_millis(200));
    let now = current_driver(bdf);
    if now.as_deref() != original {
        // Not fatal — we tried. Log via stderr so panicking callers see it.
        eprintln!(
            "warning: restore_driver({}) ended bound to {:?}, original was {:?}",
            bdf, now, original
        );
    }
    Ok(())
}

/// Enable PCI bus master on the device via VFIO config-space region. The
/// device fd's config-space region (index 7) is accessible via pread/pwrite
/// at the region's reported offset. Sets the BME bit in the Command register
/// while preserving every other bit (MEM, I/O, INTx etc.) the kernel chose
/// when it bound vfio-pci.
fn enable_bus_master(device_fd: RawFd) -> Result<(), HwError> {
    use std::os::unix::io::FromRawFd;
    // Query config region for its offset within device_fd.
    let mut region = VfioRegionInfo {
        argsz: std::mem::size_of::<VfioRegionInfo>() as u32,
        index: VFIO_PCI_CONFIG_REGION_INDEX,
        ..Default::default()
    };
    ioctl_ref(
        device_fd,
        VFIO_DEVICE_GET_REGION_INFO,
        &mut region as *mut _ as *mut libc::c_void,
    )
    .map_err(|e| HwError::Preflight(format!("config region info: {}", e)))?;

    // Borrow the device fd as a File for pread/pwrite — but DON'T let the
    // File's Drop close it (we still own device_fd via VfioBackend).
    let cfg_offset = region.offset + PCI_COMMAND_OFFSET;
    let dup = unsafe { libc::dup(device_fd) };
    if dup < 0 {
        return Err(HwError::Preflight(format!(
            "dup(device_fd) for config rw: {}",
            std::io::Error::last_os_error()
        )));
    }
    let cfg_file = unsafe { File::from_raw_fd(dup) };

    use std::os::unix::fs::FileExt;
    let mut cmd_bytes = [0u8; 2];
    cfg_file
        .read_exact_at(&mut cmd_bytes, cfg_offset)
        .map_err(|e| HwError::Preflight(format!("read PCI Command: {}", e)))?;
    let cur = u16::from_le_bytes(cmd_bytes);
    let new = cur | PCI_COMMAND_MASTER;
    eprintln!(
        "vfio: enable_bus_master cur=0x{:04x} new=0x{:04x} cfg_offset=0x{:x}",
        cur, new, cfg_offset
    );
    if new != cur {
        cfg_file
            .write_all_at(&new.to_le_bytes(), cfg_offset)
            .map_err(|e| HwError::Preflight(format!("write PCI Command: {}", e)))?;
    }
    // Read-back to verify the write stuck.
    let mut verify = [0u8; 2];
    cfg_file
        .read_exact_at(&mut verify, cfg_offset)
        .map_err(|e| HwError::Preflight(format!("verify PCI Command: {}", e)))?;
    let post = u16::from_le_bytes(verify);
    eprintln!(
        "vfio: enable_bus_master readback=0x{:04x} (BME bit {})",
        post,
        if post & PCI_COMMAND_MASTER != 0 {
            "SET"
        } else {
            "CLEAR — write was rejected!"
        }
    );
    Ok(())
}

// === thin ioctl wrappers (libc binding is verbose) =========================

fn ioctl_get(fd: RawFd, request: IoctlReq) -> std::io::Result<i32> {
    let rc = unsafe { libc::ioctl(fd, request) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn ioctl_ref(fd: RawFd, request: IoctlReq, arg: *mut libc::c_void) -> std::io::Result<i32> {
    let rc = unsafe { libc::ioctl(fd, request, arg) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

fn ioctl_value(fd: RawFd, request: IoctlReq, arg: IoctlReq) -> std::io::Result<i32> {
    let rc = unsafe { libc::ioctl(fd, request, arg) };
    if rc < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

// Silence Read/Write unused-import warnings in case future revs drop them.
#[allow(dead_code)]
fn _silence_io_traits() {
    let _: Option<&dyn Read> = None;
    let _: Option<&dyn Write> = None;
}

/// Translate a process VA to physical address via /proc/self/pagemap.
///
/// Format per Documentation/admin-guide/mm/pagemap.rst:
/// - 8 bytes per page (index = va >> page_shift)
/// - bits 0-54: page frame number (PFN); shift left by page_shift for PA
/// - bit 63: page present (must be 1; if not, kernel hasn't allocated the page)
/// - bits 56-60: page-level (used for hugepages; informational)
///
/// Returns PA of the page containing `va` plus the offset within the page.
/// Caller is responsible for ensuring `va` is mlocked or otherwise pinned so
/// the PA doesn't change between this call and the DMA.
fn va_to_pa(va: usize) -> Result<u64, String> {
    use std::io::{Read as _, Seek, SeekFrom};

    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_shift = page_size.trailing_zeros() as u64;
    let page_offset = (va as u64) & (page_size as u64 - 1);
    let pfn_index = (va as u64) >> page_shift;

    let mut f = std::fs::File::open("/proc/self/pagemap")
        .map_err(|e| format!("open /proc/self/pagemap (requires root): {}", e))?;
    f.seek(SeekFrom::Start(pfn_index * 8))
        .map_err(|e| format!("seek: {}", e))?;
    let mut buf = [0u8; 8];
    f.read_exact(&mut buf)
        .map_err(|e| format!("read pagemap entry: {}", e))?;
    let entry = u64::from_le_bytes(buf);

    let present = (entry >> 63) & 1;
    if present == 0 {
        return Err(format!(
            "page not present (pagemap entry=0x{:016x}); did you forget to mlock + touch the page?",
            entry
        ));
    }
    let pfn = entry & ((1u64 << 55) - 1);
    if pfn == 0 {
        return Err(format!(
            "PFN is zero (pagemap entry=0x{:016x}); likely running without CAP_SYS_ADMIN",
            entry
        ));
    }
    Ok((pfn << page_shift) | page_offset)
}
