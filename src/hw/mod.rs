//! Hardware backend abstraction.
//!
//! Verbs that talk to the chip (detect, backup, recover, flash, sbr) operate
//! through this trait. Implementations handle the platform specifics:
//!
//! - `VfioBackend` — production. Binds card to vfio-pci, maps BAR1 + DMA via
//!   `/dev/vfio`. Auto-restore on Drop.
//! - `MockBackend` — tests. Backing storage is `Vec<u8>`; IOVA is fake.
//!
//! Why this exists: chip-side code (MPI, doorbell, FW_UPLOAD/DOWNLOAD) should
//! not know whether DMA buffers came from VFIO, hugepage+pagemap, or a custom
//! kernel module. The trait makes that decision pluggable. Today VFIO is the
//! only production backend; if a future host environment requires something
//! else (no IOMMU module, pre-2012 kernel, hypervisor passthrough quirk), add
//! a new `HwBackend` impl without touching MPI code.

use std::path::PathBuf;
use thiserror::Error;

pub mod vfio;

#[derive(Debug, Error)]
pub enum HwError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("no usable hardware backend (tried: {tried})")]
    NoBackend { tried: String },

    #[error("driver bind/unbind: {0}")]
    DriverBind(String),

    #[error("VFIO group not available for {bdf}: {reason}")]
    VfioGroupUnavailable { bdf: String, reason: String },

    #[error("BAR1 mmap failed: {0}")]
    Bar1Mmap(String),

    #[error("DMA allocation failed: {0}")]
    DmaAlloc(String),

    #[error("kernel module missing: {0}")]
    ModuleMissing(&'static str),

    #[error("preflight: {0}")]
    Preflight(String),
}

/// A region of memory the chip can DMA to/from.
///
/// `va` is what the host reads/writes (a regular virtual pointer into our
/// process's address space). `iova` is what goes into MPI Scatter-Gather
/// Elements (SGEs) the chip reads when DMA'ing. On no-IOMMU systems `iova`
/// is a physical address; on IOMMU systems it's an IO-virtual-address the
/// IOMMU translates.
///
/// Lifetime: lives as long as the backend that created it. Backend frees on
/// Drop or when the `DmaHandle` opaque field is closed via `free_dma`.
pub struct DmaBuffer {
    /// Host-side virtual pointer. Length is `len`. Valid for `read`/`write`
    /// through `unsafe { std::slice::from_raw_parts_mut(va, len) }`.
    pub va: *mut u8,

    /// Address the chip sees. Goes into MPI SGE.
    pub iova: u64,

    /// Length in bytes.
    pub len: usize,

    /// Opaque to callers; backend uses for free.
    pub handle: u64,
}

// DmaBuffer is `Send` because the underlying mmap is process-wide. The
// raw pointer isn't `Sync` — callers should not share a single buffer
// across threads without coordination.
unsafe impl Send for DmaBuffer {}

/// Hardware backend — abstraction over how we talk to the PCIe device.
///
/// Implementations must restore system state on Drop (rebind original driver,
/// remount things that depended on it, etc.). Callers should rely on RAII;
/// the only explicit `close` path is for error reporting during the unbind.
pub trait HwBackend: Send {
    /// Read-write slice of BAR1. Lifetime-bound to `self`.
    fn bar1(&mut self) -> &mut [u8];

    /// Allocate DMA-coherent memory of at least `len` bytes. May return more
    /// (rounded up to a hugepage boundary). The returned `DmaBuffer` must be
    /// freed via `free_dma` before the backend is dropped, OR the backend's
    /// Drop will clean up all outstanding buffers.
    fn alloc_dma(&mut self, len: usize) -> Result<DmaBuffer, HwError>;

    /// Release a DMA buffer previously returned by `alloc_dma`.
    fn free_dma(&mut self, buf: DmaBuffer) -> Result<(), HwError>;

    /// Human-readable backend name for logging ("vfio", "mock", etc.).
    fn name(&self) -> &'static str;

    /// PCI BDF this backend is bound to.
    fn bdf(&self) -> &str;
}

/// Auto-detect the right backend for this host + bdf.
///
/// Order:
/// 1. VFIO — try first. Standard answer.
/// 2. (future) other backends could be added here.
///
/// Errors with `NoBackend` if nothing works. The error string lists what was
/// tried and why each failed so the operator can fix their environment.
pub fn auto_detect(bdf: &str) -> Result<Box<dyn HwBackend>, HwError> {
    let mut tried = Vec::new();

    match vfio::VfioBackend::open(bdf) {
        Ok(be) => return Ok(Box::new(be)),
        Err(e) => tried.push(format!("vfio: {}", e)),
    }

    Err(HwError::NoBackend {
        tried: tried.join("; "),
    })
}

/// Path to the PCI device's sysfs directory. Used by backends to walk
/// `driver`, `iommu_group`, `resource1`, etc. Step 2 will exercise this.
#[allow(dead_code)]
pub(crate) fn pci_sysfs_dir(bdf: &str) -> PathBuf {
    PathBuf::from(format!("/sys/bus/pci/devices/{}", bdf))
}
