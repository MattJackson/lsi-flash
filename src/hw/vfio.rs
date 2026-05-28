//! VFIO-backed hardware access.
//!
//! Layered on top of two stock Linux kernel modules:
//!   - `vfio`     — provides the `/dev/vfio/vfio` container device
//!   - `vfio-pci` — meta-driver that lets userspace own a PCI device
//!
//! Both ship in every mainstream distro's kernel. `lsi-flash` runtime loads
//! `vfio-pci` if not already present (with `enable_unsafe_noiommu_mode=Y`
//! when IOMMU is off — required on dev-1 today).
//!
//! Lifecycle: `open(bdf)` does the unbind-from-mpt3sas / bind-to-vfio-pci
//! dance, opens the VFIO container + group, maps BAR1. `Drop` reverses it.
//!
//! Citations:
//!   - kernel docs: Documentation/driver-api/vfio.rst (VFIO_GET_API_VERSION,
//!     VFIO_SET_IOMMU, VFIO_GROUP_GET_DEVICE_FD, VFIO_IOMMU_MAP_DMA, etc.)
//!   - mpt3sas + vfio-pci coexistence pattern: same as dpdk-devbind.py,
//!     virsh nodedev-detach, GPU passthrough — well-trodden.
//!
//! Status: SCAFFOLD ONLY in this commit. Step 1 of 7-step VFIO rollout.
//! `open()` returns Err for now until the bind dance is implemented.

use super::{DmaBuffer, HwBackend, HwError};

pub struct VfioBackend {
    bdf: String,
    // TODO step 2: container_fd, group_fd, device_fd
    // TODO step 2: bar1_mmap (Option<...>)
    // TODO step 3: live DMA mappings for cleanup on Drop
    // TODO step 6: original_driver (Option<String>) — what mpt3sas was named so
    //              we can rebind exactly on Drop
}

impl VfioBackend {
    pub fn open(bdf: &str) -> Result<Self, HwError> {
        Err(HwError::NoBackend {
            tried: format!("vfio: not yet implemented (open '{}')", bdf),
        })
    }
}

impl HwBackend for VfioBackend {
    fn bar1(&mut self) -> &mut [u8] {
        unimplemented!("vfio step 2 — BAR1 mmap via VFIO_DEVICE_GET_REGION_INFO")
    }

    fn alloc_dma(&mut self, _len: usize) -> Result<DmaBuffer, HwError> {
        Err(HwError::DmaAlloc(
            "vfio step 3 — VFIO_IOMMU_MAP_DMA not yet implemented".into(),
        ))
    }

    fn free_dma(&mut self, _buf: DmaBuffer) -> Result<(), HwError> {
        Err(HwError::DmaAlloc(
            "vfio step 3 — VFIO_IOMMU_UNMAP_DMA not yet implemented".into(),
        ))
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
        // TODO step 6: unbind vfio-pci, rebind original driver (mpt3sas)
        // Must be idempotent — drop may run from panic / SIGINT handler.
    }
}
