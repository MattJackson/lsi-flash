//! HCB (Host Code Boot) sequence for lsi-flash.
//! Port of lsirec.c:225-297 (setup_hcdw), 794-860 (do_hostboot).

use crate::mpi::diag::{halt, MpiError};
use crate::mpi::doorbell::{read32, write32, MpiRegisterContext};
use std::io::{Read, Seek};
use std::os::unix::fs::FileExt;
use thiserror::Error;

// MAP_HUGETLB and MAP_LOCKED constants for mmap. Cites lsirec.c:260-268.
const MAP_HUGETLB: libc::c_int = 0x40000;
const MAP_LOCKED: libc::c_int = 0x2000;

/// HCDW buffer size (2 MB). Cites lsirec.c:70.
pub const HCDW_SIZE: usize = 0x200000; // 2 * 1024 * 1024 bytes

/// HCDW address low register offset (BAR1 + 0x78). Cites lsirec.c:47.
pub const MPI2_HCDW_ADDR_LOW: u32 = 0x78;

/// HCDW address high register offset (BAR1 + 0x7C). Cites lsirec.c:48.
pub const MPI2_HCDW_ADDR_HIGH: u32 = 0x7c;

/// HCDW size register offset (BAR1 + 0x74). Cites lsirec.c:43-46.
pub const MPI2_HCDW_SIZE: u32 = 0x74;

/// HCDW size mask for physical address. Cites lsirec.c:44.
pub const MPI2_HCDW_SIZE_SIZE_MASK: u32 = 0xFFFFF000;

/// HCDW size enable bit (HCB mode). Cites lsirec.c:45-46.
pub const MPI2_HCDW_SIZE_HCB_ENABLE: u32 = 0x00000001;

/// Error type for HCB operations.
#[derive(Error, Debug)]
pub enum HcbError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Hugepage allocation failed: {0}")]
    HugepageAllocationFailed(std::io::Error),

    #[error("Insufficient hugepages (need {required}, have {available})")]
    InsufficientHugepages { required: u64, available: u64 },

    #[error("IOC did not become ready after boot (doorbell=0x{doorbell:08x})")]
    IocNotReadyAfterBoot { doorbell: u32 },

    #[error("Bus mastering enable not implemented — needs BDF parameter")]
    BusMasteringNotImplemented,

    #[error("Pagemap read failed")]
    PagemapReadFailed,

    #[error("MPI error: {0}")]
    Mpi(#[from] MpiError),

    #[error("Parse error: {0}")]
    Parse(#[from] std::num::ParseIntError),
}

/// Allocate HCDW buffer with hugepages. Cites lsirec.c:260-268.
pub fn allocate_hcdw_buffer() -> Result<Box<[u8; HCDW_SIZE]>, HcbError> {
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            HCDW_SIZE,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | MAP_HUGETLB | MAP_LOCKED,
            -1,
            0,
        )
    };

    if ptr == libc::MAP_FAILED {
        let err = std::io::Error::last_os_error();
        eprintln!("mmap hcdw failed: {}", err);
        eprintln!("Do you have hugepages enabled?");
        eprintln!("Try: echo 16 > /proc/sys/vm/nr_hugepages");
        return Err(HcbError::HugepageAllocationFailed(err));
    }

    let slice = unsafe { std::slice::from_raw_parts_mut(ptr as *mut u8, HCDW_SIZE) };
    let mut buffer: [u8; HCDW_SIZE] = [0; HCDW_SIZE];
    buffer.copy_from_slice(slice);

    Ok(Box::new(buffer))
}

/// Resolve physical address from pagemap. Cites lsirec.c:270-291.
pub fn resolve_physical_address(buffer: &[u8; HCDW_SIZE]) -> Result<u64, HcbError> {
    let mut pagemap = std::fs::File::open("/proc/self/pagemap")?;

    let page_index = (buffer.as_ptr() as usize) >> 12; // PAGE_SHIFT = 12
    let byte_offset = (page_index * 8) as u64;

    pagemap.seek(std::io::SeekFrom::Start(byte_offset))?;

    let mut entry: u64 = 0;
    pagemap.read_exact(unsafe {
        std::slice::from_raw_parts_mut(&mut entry as *mut u64 as *mut u8, 8)
    })?;

    let pfn = entry & ((1u64 << 55) - 1);
    let physical_address = pfn << 12;

    Ok(physical_address)
}

/// Setup HCDW with firmware loaded. Cites lsirec.c:225-297.
pub fn setup_hcdw(
    bar1: &mut [u8; 4096],
    _ctx: &MpiRegisterContext,
    buffer: &mut [u8; HCDW_SIZE],
) -> Result<(), HcbError> {
    println!("Setting up HCB...");

    enable_bus_mastering()?; // TODO: full implementation needs BDF

    let phys_addr = resolve_physical_address(buffer)?;
    println!("HCDW virtual: {:p}", buffer.as_ptr());
    println!("HCDW physical: 0x{:x}", phys_addr);

    write32(bar1, MPI2_HCDW_ADDR_LOW, (phys_addr & 0xffffffff) as u32);
    write32(bar1, MPI2_HCDW_ADDR_HIGH, (phys_addr >> 32) as u32);

    let size_val = (0xfffff000usize & !(HCDW_SIZE - 1)) | (MPI2_HCDW_SIZE_HCB_ENABLE as usize);
    write32(bar1, MPI2_HCDW_SIZE, size_val as u32);

    Ok(())
}

/// Load firmware into HCDW buffer at end of region. Cites lsirec.c:817-828.
fn setup_hcdw_with_firmware(
    bar1: &mut [u8; 4096],
    ctx: &MpiRegisterContext,
    buffer: &mut [u8; HCDW_SIZE],
    firmware_path: &str,
) -> Result<(), HcbError> {
    buffer.fill(0x42);

    let fd = std::fs::File::open(firmware_path)?;
    let length = fd.read_at(buffer, 0u64)?;
    println!("Loaded {} bytes", length);

    // Move firmware to end of HCDW region (lsirec.c:827 memmove pattern)
    unsafe {
        let src_ptr = buffer.as_mut_ptr();
        let dst_ptr = src_ptr.add(HCDW_SIZE - length);
        std::ptr::copy(src_ptr, dst_ptr, length);
    }

    setup_hcdw(bar1, ctx, buffer)?;

    Ok(())
}

/// Boot firmware via HCB. Cites lsirec.c:794-860.
pub fn hostboot(
    bar1: &mut [u8; 4096],
    ctx: &MpiRegisterContext,
    buffer: &mut [u8; HCDW_SIZE],
    firmware_path: &str,
) -> Result<(), HcbError> {
    // Step 1: Halt IOC (lsirec.c:799).
    halt(bar1, ctx)?;

    // Step 2: Clear flash bad sig and set boot device to HCDW (lsirec.c:803-808).
    let mut val = read32(bar1, ctx.diag_offset);
    val |= crate::mpi::doorbell::MPI2_DIAG_CLR_FLASH_BAD_SIG;
    val &= !crate::mpi::doorbell::MPI2_DIAG_BOOTDEVICE_MASK;
    val &= !crate::mpi::doorbell::MPI2_DIAG_FORCE_HCB;
    val &= !crate::mpi::doorbell::MPI2_DIAG_RESET_HISTORY;
    write32(bar1, ctx.diag_offset, val);

    // Step 3: Setup HCDW with firmware (lsirec.c:809-854).
    setup_hcdw_with_firmware(bar1, ctx, buffer, firmware_path)?;

    // Step 4: Set boot device to HCDW (lsirec.c:829-831).
    val = read32(bar1, ctx.diag_offset);
    val |= crate::mpi::doorbell::MPI2_DIAG_BOOTDEVICE_HCDW;
    write32(bar1, ctx.diag_offset, val);

    // Step 5: Release IOC from reset (lsirec.c:834-839).
    val &= !crate::mpi::doorbell::MPI2_DIAG_HOLD_IOC_RESET;
    val &= !crate::mpi::doorbell::MPI2_DIAG_FORCE_HCB;
    write32(bar1, ctx.diag_offset, val);

    println!("Booting IOC...");

    // Step 6: Wait for READY state (lsirec.c:841-847).
    for _ in 0..200 {
        let db = read32(bar1, crate::mpi::doorbell::MPI2_DOORBELL);
        if db & crate::mpi::doorbell::MPI2_DOORBELL_READY != 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    // Step 7: Verify READY state (lsirec.c:849-853).
    let db = read32(bar1, crate::mpi::doorbell::MPI2_DOORBELL);
    if db & crate::mpi::doorbell::MPI2_DOORBELL_READY == 0 {
        return Err(HcbError::IocNotReadyAfterBoot { doorbell: db });
    }

    println!(
        "IOC state after boot: {:?}",
        crate::mpi::doorbell::get_ioc_state(bar1, crate::mpi::doorbell::MPI2_DOORBELL)
    );

    // Step 8: Wait 500ms for IOC to initialize (lsirec.c:856).
    std::thread::sleep(std::time::Duration::from_millis(500));

    println!("IOC Host Boot successful.");

    Ok(())
}

/// Check if hugepages are available. Cites scoping doc §4 risk #2.
pub fn check_hugepages() -> Result<(), HcbError> {
    let nr_hugepages = std::fs::read_to_string("/proc/sys/vm/nr_hugepages")?;
    let nr_hugepages: u64 = nr_hugepages.trim().parse()?;

    if nr_hugepages < 16 {
        return Err(HcbError::InsufficientHugepages {
            required: 16,
            available: nr_hugepages,
        });
    }

    Ok(())
}

/// Check for IOMMU (HCB is incompatible with active IOMMU). Cites scoping doc §4 risk #3.
pub fn check_iommu() -> Result<(), HcbError> {
    let iommu_dir = std::path::PathBuf::from("/sys/kernel/iommu_groups");

    if iommu_dir.exists() {
        eprintln!("WARNING: IOMMU detected at /sys/kernel/iommu_groups");
        eprintln!("HCB may fail if IOMMU is active.");
        eprintln!("Try disabling with kernel cmdline: iommu=off intel_iommu=off");
    }

    Ok(())
}

/// Enable bus mastering via PCI config space. Cites lsirec.c:232-258.
fn enable_bus_mastering() -> Result<(), HcbError> {
    // TODO: implement full pattern — needs BDF parameter from caller.
    Err(HcbError::BusMasteringNotImplemented)
}
