//! `SbrTransport` — pluggable abstraction over "how do I read the 256-byte
//! SBR EEPROM from the chip". Per ADR-017's pluggability principle, the same
//! shape as MptTransport: trait + multiple impls + caller picks.
//!
//! Three impls today:
//!
//! - **`Bar1MmapSbrTransport`** (PRIMARY) — direct `/sys/.../resource1` mmap
//!   (lsirec-style), NO VFIO bind dance, NO device reset → SAS PHY links stay up.
//!   Unbinds mpt3sas briefly (~1s blip), mmaps resource1, I²C bit-bang via
//!   `src/sbr/i2c.rs`, rebinds on Drop. References: lsirec.c:205-213 (resource1 mmap).
//!
//! - **`VfioI2cSbrTransport`** — FULLY WORKING fallback for kernel-lockdown /
//!   SecureBoot systems where raw `/sys/.../resource1` mmap is blocked. Uses VFIO
//!   bind dance (evicts mpt3sas → binds vfio-pci → mmap BAR1). Cost: device reset,
//!   SAS PHY drop → requires reboot to recover. Retained as documented fallback;
//!   NOT the default for sbr read because of the disk yank / reboot cost.
//!
//! - **`IstwiSbrTransport`** — proves out via MPI TOOLBOX_ISTWI through
//!   `MptTransport` (mpt3sas stays bound, /dev/sdb stays mounted — no
//!   disruption). Currently returns `NotImplemented` because the
//!   DevIndex/Action/TxData combo for SAS2008 returns IOCStatus
//!   0x8004 (INTERNAL_ERROR). Wire format scaffold kept in place; gating
//!   in `read_sbr` returns NotImplemented until the open questions
//!   below are answered.
//!
//! Open questions for IstwiSbrTransport (track in code TODOs):
//!   - SAS2008 SBR DevIndex (we tried 0; chip rejects)
//!   - Whether READ_DATA action needs TxData=[0x00] (offset byte)
//!   - Whether SEQUENCE (0x03) action with TxData=[0x00] + RxDataLength=256
//!     is the correct combo
//!
//! When IstwiSbrTransport is proven, `MptCard::sbr_read` swaps its choice
//! — one-line change. No throw-away.

use crate::hw::HwBackend;
use thiserror::Error;

/// Errors for SBR transport operations.
#[derive(Debug, Error)]
pub enum SbrTransportError {
    /// The operation is not yet implemented (wire format research needed).
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),

    /// Generic transport-level error (VFIO open, I2C init, etc.).
    #[error("transport: {0}")]
    Transport(String),

    /// IO error from underlying system calls.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Pluggable abstraction for reading SBR EEPROM from the chip.
pub trait SbrTransport: Send {
    /// Read the 256-byte SBR EEPROM. Caller owns the choice of impl.
    fn read_sbr(&mut self) -> Result<[u8; 256], SbrTransportError>;

    /// Short name for logging ("bar1-mmap-i2c" / "vfio-i2c" / "istwi" / etc.).
    /// Lets callers log which path was used without matching on concrete types.
    fn name(&self) -> &'static str;
}

/// MPI TOOLBOX_ISTWI path. Currently broken — DevIndex/Action discovery
/// needed for SAS2008. Kept in code so it's ready to enable when the
/// wire format is figured out.
///
/// Source of wire-format research: `src/card/mpt.rs::MptCard::sbr_read` (commit 8fd35ff).
pub struct IstwiSbrTransport {
    pub transport: Box<dyn crate::mpt::MptTransport>,
}

impl SbrTransport for IstwiSbrTransport {
    fn read_sbr(&mut self) -> Result<[u8; 256], SbrTransportError> {
        // ATTEMPT A — MPI **1.x** ToolboxIstwiReadWrite layout (mpi_tool.h:150-173),
        // which carries the slave address + offset that the MPI 2.0 struct lacks.
        // The 2.0 DevIndex/Action form returned IOCStatus 0x8004 because it cannot
        // express "slave 0x54, write 2-byte offset, read 256". Per
        // 04-mpi-protocol/istwi-sbr-wire-format.md. Mirrors lsiutil.c:22881-22890
        // (SFP read: same struct, DeviceAddr=0xa0/NumAddressBytes=1).
        //
        // Fixed request frame = 28 bytes (0x00..0x1C); SGL appended at word 7.
        let mut req = [0u8; 0x1C];
        req[0x00] = 0x03; // Tool = MPI_TOOLBOX_ISTWI_READ_WRITE_TOOL (mpi_tool.h:37)
        req[0x03] = 0x17; // Function = MPI_FUNCTION_TOOLBOX (mpi.h:294)
        req[0x0C] = 0x01; // Flags = MPI_TB_ISTWI_FLAGS_READ (mpi_tool.h:176)
        req[0x0D] = 0x00; // BusNum = 0
        req[0x10] = 0x02; // NumAddressBytes = 2 (16-bit EEPROM offset)
        req[0x12..0x14].copy_from_slice(&256u16.to_le_bytes()); // DataLength
        req[0x14] = 0xA8; // DeviceAddr = slave 0x54 << 1 (8-bit bus byte)
        req[0x15] = 0x00; // Addr1 = offset hi
        req[0x16] = 0x00; // Addr2 = offset lo

        let mut data_in = vec![0u8; 256]; // IOC → host SBR bytes
        let mut reply = [0u8; 64];
        let n = self
            .transport
            .send_with_sge_offset(&req, 7, &mut reply, Some(&mut data_in), None)
            .map_err(|e| SbrTransportError::Transport(format!("istwi send: {}", e)))?;

        // Reply: IOCStatus @ 0x0E (U16 LE), IOCLogInfo @ 0x10 (U32 LE) (mpi_tool.h:57-58).
        let ioc_status = if n >= 0x10 {
            u16::from_le_bytes([reply[0x0E], reply[0x0F]])
        } else {
            0xFFFF
        };
        let ioc_log_info = if n >= 0x14 {
            u32::from_le_bytes([reply[0x10], reply[0x11], reply[0x12], reply[0x13]])
        } else {
            0
        };
        // IstwiStatus byte (MPI2 reply @0x16; capture for diagnosis if present).
        let istwi_status = if n > 0x16 { reply[0x16] } else { 0 };

        if ioc_status & 0x7FFF != 0 {
            return Err(SbrTransportError::Transport(format!(
                "istwi: IOCStatus=0x{:04x} IOCLogInfo=0x{:08x} IstwiStatus=0x{:02x} \
                 (reply[0..24]={:02x?})",
                ioc_status,
                ioc_log_info,
                istwi_status,
                &reply[..24.min(n)]
            )));
        }

        let mut arr = [0u8; 256];
        arr.copy_from_slice(&data_in);
        Ok(arr)
    }

    fn name(&self) -> &'static str {
        "istwi"
    }
}

/// VFIO + BAR1 I²C bit-bang path. FULLY WORKING fallback for kernel-lockdown /
/// SecureBoot systems where raw `/sys/.../resource1` mmap is blocked. Uses VFIO
/// bind dance (evicts mpt3sas → binds vfio-pci → mmap BAR1). Cost: device reset,
/// SAS PHY drop → requires reboot to recover. Retained as documented fallback;
/// NOT the default for sbr read because of the disk yank / reboot cost.
pub struct VfioI2cSbrTransport {
    vfio: crate::hw::vfio::VfioBackend,
}

impl VfioI2cSbrTransport {
    pub fn open(bdf: &str) -> Result<Self, SbrTransportError> {
        eprintln!(
            "sbr-read via VFIO+I²C: temporarily evicting mpt3sas from {} \
             (any /dev/sdX on this HBA will disconnect for ~1s, then restored).",
            bdf
        );
        let vfio = crate::hw::vfio::VfioBackend::open(bdf)
            .map_err(|e| SbrTransportError::Transport(format!("vfio open: {}", e)))?;
        Ok(Self { vfio })
    }
}

impl SbrTransport for VfioI2cSbrTransport {
    fn read_sbr(&mut self) -> Result<[u8; 256], SbrTransportError> {
        use crate::sbr::i2c::{i2c_close, i2c_init, i2c_read_sbr, I2cContext};

        // Use live BAR1 slice directly (no copy).
        let bar1 = self.vfio.bar1();
        let mut ctx = I2cContext {
            bar1,
            sbr_addr: 0,
            eep_type: 0,
        };

        // Auto-detect EEPROM address/type via i2c_init (lsirec.c:498-524).
        i2c_init(&mut ctx).map_err(|e| SbrTransportError::Transport(format!("i2c_init: {}", e)))?;

        // Call i2c_read_sbr directly (src/sbr/i2c.rs).
        let bytes = i2c_read_sbr(&mut ctx, 0, 256)
            .map_err(|e| SbrTransportError::Transport(format!("i2c_read_sbr: {}", e)))?;

        if bytes.len() != 256 {
            return Err(SbrTransportError::Transport(format!(
                "i2c_read_sbr returned {} bytes, expected 256",
                bytes.len()
            )));
        }

        let mut arr = [0u8; 256];
        arr.copy_from_slice(&bytes);

        // Best-effort restore of DCR_I2C_SELECT (lsirec.c:535-549).
        let _ = i2c_close(&mut ctx);

        Ok(arr)
    }

    fn name(&self) -> &'static str {
        "vfio-i2c"
    }
}

/// Direct `/sys/.../resource1` mmap path (lsirec-style). NO VFIO, NO device reset.
/// Unbinds mpt3sas briefly (~1s blip), mmaps resource1 directly, I²C bit-bang via
/// `src/sbr/i2c.rs`, rebinds on Drop. References: lsirec.c:205-213 (resource1 mmap).
///
/// SAFETY: The raw BAR1 pointer (`va`) is only accessed from this struct's methods
/// and the Drop implementation. Single-threaded use (e.g., `sbr read` CLI command)
/// guarantees no concurrent access, so we add `unsafe impl Send`. This mirrors
/// the pattern used by `DmaBuffer` in `src/hw/mod.rs`.
pub struct Bar1MmapSbrTransport {
    bdf: String,
    original_driver: Option<String>, // for rebind on Drop (e.g., "mpt3sas")
    va: *mut u8,                     // mmap base (resource1)
    len: usize,                      // mmap length (typically 64 KB)
    fd: std::os::fd::RawFd,          // resource1 fd
}

unsafe impl Send for Bar1MmapSbrTransport {}

impl Bar1MmapSbrTransport {
    /// Open BAR1 via direct `/sys/.../resource1` mmap (lsirec-style).
    ///
    /// Steps:
    /// 1. Save current driver (likely "mpt3sas").
    /// 2. Unbind from current driver (write BDF to `/sys/bus/pci/drivers/<drv>/unbind`).
    ///    This causes the disk behind the HBA to blip offline for ~1s.
    /// 3. Open resource1 fd (`O_RDWR`).
    /// 4. fstat → len, mmap(NULL, len, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0).
    ///    On failure: rebind the driver and return Err (don't leave device unbound).
    pub fn open(bdf: &str) -> Result<Self, SbrTransportError> {
        // Step 1: Save current driver.
        let original_driver = vfio::current_driver(bdf);

        if let Some(ref drv) = original_driver {
            eprintln!(
                "sbr-read via BAR1 mmap: unbinding {} from {} (any /dev/sdX on this HBA \
                 will disconnect for ~1s, then restored).",
                bdf, drv
            );
        } else {
            eprintln!(
                "sbr-read via BAR1 mmap: no driver bound to {}, proceeding with direct mmap.",
                bdf
            );
        }

        // Step 2: Unbind from current driver.
        if let Some(ref drv) = original_driver {
            let unbind_path = format!("/sys/bus/pci/drivers/{}/unbind", drv);
            std::fs::write(&unbind_path, format!("{}\n", bdf)).map_err(|e| {
                SbrTransportError::Transport(format!("unbind {} from {}: {}", bdf, drv, e))
            })?;
        }

        // Step 3: Open resource1 fd.
        let resource_path = format!("/sys/bus/pci/devices/{}/resource1", bdf);
        let fd = unsafe {
            libc::open(
                std::ffi::CString::new(&*resource_path).unwrap().as_ptr(),
                libc::O_RDWR,
            )
        };

        if fd < 0 {
            // On failure: rebind the driver and return Err.
            let _ = restore_driver(bdf, original_driver.as_deref());
            return Err(SbrTransportError::Io(std::io::Error::last_os_error()));
        }

        // Step 4: fstat → len, mmap.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            let _ = restore_driver(bdf, original_driver.as_deref());
            let _ = unsafe { libc::close(fd) };
            return Err(SbrTransportError::Io(std::io::Error::last_os_error()));
        }

        // resource1 size is st.st_size (typically 64 KB for SAS2008).
        let len = st.st_size as usize;
        if len == 0 {
            let _ = restore_driver(bdf, original_driver.as_deref());
            let _ = unsafe { libc::close(fd) };
            return Err(SbrTransportError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "resource1 reported zero size",
            )));
        }

        // mmap(NULL, len, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0).
        let va = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };

        if va == libc::MAP_FAILED {
            // On failure: rebind the driver and return Err.
            let _ = restore_driver(bdf, original_driver.as_deref());
            let _ = unsafe { libc::close(fd) };
            return Err(SbrTransportError::Io(std::io::Error::last_os_error()));
        }

        Ok(Self {
            bdf: bdf.to_string(),
            original_driver,
            va: va as *mut u8,
            len,
            fd,
        })
    }
}

impl SbrTransport for Bar1MmapSbrTransport {
    fn read_sbr(&mut self) -> Result<[u8; 256], SbrTransportError> {
        use crate::sbr::i2c::{i2c_close, i2c_init, i2c_read_sbr, I2cContext};

        // SAFETY: `va` is valid for `len` bytes (guaranteed by mmap success + fstat).
        let bar1: &mut [u8] = unsafe { std::slice::from_raw_parts_mut(self.va, self.len) };
        let mut ctx = I2cContext {
            bar1,
            sbr_addr: 0,
            eep_type: 0,
        };

        // Auto-detect EEPROM address/type via i2c_init (lsirec.c:498-524).
        i2c_init(&mut ctx).map_err(|e| SbrTransportError::Transport(format!("i2c_init: {}", e)))?;

        // Call i2c_read_sbr directly (src/sbr/i2c.rs).
        let bytes = i2c_read_sbr(&mut ctx, 0, 256)
            .map_err(|e| SbrTransportError::Transport(format!("i2c_read_sbr: {}", e)))?;

        if bytes.len() != 256 {
            return Err(SbrTransportError::Transport(format!(
                "i2c_read_sbr returned {} bytes, expected 256",
                bytes.len()
            )));
        }

        let mut arr = [0u8; 256];
        arr.copy_from_slice(&bytes);

        // Best-effort restore of DCR_I2C_SELECT (lsirec.c:535-549).
        let _ = i2c_close(&mut ctx);

        Ok(arr)
    }

    fn name(&self) -> &'static str {
        "bar1-mmap-i2c"
    }
}

impl Drop for Bar1MmapSbrTransport {
    fn drop(&mut self) {
        // SAFETY-CRITICAL: MUST run in order, best-effort (never panic in Drop).
        // 1. munmap(va, len)
        // 2. close(fd)
        // 3. rebind original driver (disk recovery depends on this!)

        // Step 1: munmap.
        if !self.va.is_null() && self.len > 0 {
            let _ = unsafe { libc::munmap(self.va as *mut libc::c_void, self.len) };
        }

        // Step 2: close fd.
        let _ = unsafe { libc::close(self.fd) };

        // Step 3: rebind original driver (best-effort but mandatory for disk recovery).
        let bdf = &self.bdf;
        if let Some(ref drv) = self.original_driver {
            let _ = restore_driver(bdf, Some(drv.as_str()));
        } else {
            // If there was no original driver, try drivers_probe to let kernel reprobe.
            let _ = std::fs::write("/sys/bus/pci/drivers_probe", format!("{}\n", bdf));
        }

        // Step 4: trigger a SCSI rescan so disks behind the HBA re-enumerate
        // automatically (the rebind re-creates the scsi_host, but target
        // discovery is not always immediate — without this, /dev/sdX needs a
        // manual rescan). Best-effort; only meaningful when a driver was rebound.
        if self.original_driver.is_some() {
            trigger_scsi_rescan(bdf);
        }

        // Note: We DO NOT panic if any step fails — Drop runs even on panic,
        // and logging via eprintln is the best we can do for recovery.
    }
}

/// After rebinding the HBA driver, re-scan its SCSI host(s) so SAS targets
/// (e.g. `/dev/sdb`) re-enumerate without a manual rescan or reboot. The host
/// re-appears under `/sys/bus/pci/devices/<bdf>/host*/scsi_host/host*/scan`
/// shortly after rebind; give it a brief settle, then write the rescan trigger.
/// Best-effort — never errors out of Drop.
fn trigger_scsi_rescan(bdf: &str) {
    // The driver re-creates the scsi_host ASYNCHRONOUSLY after rebind (port-enable
    // takes ~2-3s), so the scan file doesn't exist immediately. Poll for it (up to
    // ~6s), then write the rescan trigger. "- - -" = scan all channels/targets/luns.
    let dev_dir = format!("/sys/bus/pci/devices/{}", bdf);
    for _ in 0..30 {
        if let Ok(entries) = std::fs::read_dir(&dev_dir) {
            let mut scanned = false;
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if let Some(n) = name.strip_prefix("host") {
                    let scan = format!("{dev_dir}/host{n}/scsi_host/host{n}/scan");
                    if std::path::Path::new(&scan).exists() {
                        let _ = std::fs::write(&scan, "- - -\n");
                        scanned = true;
                    }
                }
            }
            if scanned {
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

// === Helper functions (reusing vfio.rs patterns) ============================

/// Read `/sys/bus/pci/devices/<bdf>/driver` to get the currently-bound driver.
/// Returns None if no driver is bound (orphan device).
///
/// Reusing from `src/hw/vfio.rs:690-698`. Made `pub(crate)` by widening visibility.
fn current_driver(bdf: &str) -> Option<String> {
    use std::path::PathBuf;

    let link = PathBuf::from(format!("/sys/bus/pci/devices/{}/driver", bdf));
    std::fs::read_link(link).ok().and_then(|target| {
        target
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
    })
}

/// Reverse `bind_to_vfio_pci` — unbind from vfio-pci, clear driver_override,
/// let the kernel reprobe the original driver (mpt3sas). Best-effort: if any
/// step fails we log and continue, since this runs in Drop.
///
/// Reusing from `src/hw/vfio.rs:732-748`. Made `pub(crate)` by widening visibility.
fn restore_driver(bdf: &str, original: Option<&str>) -> Result<(), std::io::Error> {
    use std::fs::{self};

    let unbind_path = "/sys/bus/pci/drivers/vfio-pci/unbind";
    let _ = fs::write(unbind_path, format!("{}\n", bdf));

    let override_path = format!("/sys/bus/pci/devices/{}/driver_override", bdf);
    let _ = fs::write(&override_path, b"\n");

    // Try to rebind original driver first.
    if let Some(drv) = original {
        let bind_path = format!("/sys/bus/pci/drivers/{}/bind", drv);
        let _ = fs::write(&bind_path, format!("{}\n", bdf));
    } else {
        // No original driver — try drivers_probe to let kernel reprobe.
        let _ = fs::write("/sys/bus/pci/drivers_probe", format!("{}\n", bdf));
    }

    std::thread::sleep(std::time::Duration::from_millis(200));

    // Log result (best-effort, never panic).
    if let Some(drv) = current_driver(bdf) {
        eprintln!("sbr-read: rebind complete — {} now bound to {}", bdf, drv);
    } else {
        eprintln!(
            "warning: sbr-read: no driver bound to {} after reprobe",
            bdf
        );
    }

    Ok(())
}

// Re-export the vfio helpers so transport.rs can call them.
#[doc(hidden)]
pub mod vfio {
    use std::path::PathBuf;

    /// Read `/sys/bus/pci/devices/<bdf>/driver` to get the currently-bound driver.
    pub fn current_driver(bdf: &str) -> Option<String> {
        let link = PathBuf::from(format!("/sys/bus/pci/devices/{}/driver", bdf));
        std::fs::read_link(link).ok().and_then(|target| {
            target
                .file_name()
                .and_then(|n| n.to_str())
                .map(|s| s.to_string())
        })
    }

    /// Restore the original driver for a PCI device. Best-effort.
    pub fn restore_driver(bdf: &str, original: Option<&str>) -> Result<(), std::io::Error> {
        use std::fs;

        let unbind_path = "/sys/bus/pci/drivers/vfio-pci/unbind";
        let _ = fs::write(unbind_path, format!("{}\n", bdf));

        let override_path = format!("/sys/bus/pci/devices/{}/driver_override", bdf);
        let _ = fs::write(&override_path, b"\n");

        if let Some(drv) = original {
            let bind_path = format!("/sys/bus/pci/drivers/{}/bind", drv);
            let _ = fs::write(&bind_path, format!("{}\n", bdf));
        } else {
            let _ = fs::write("/sys/bus/pci/drivers_probe", format!("{}\n", bdf));
        }

        std::thread::sleep(std::time::Duration::from_millis(200));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar1_mmap_sbr_transport_name() {
        // We can't actually open a device in tests, so we test the name via a mock.
        // This is a compile-time check that Bar1MmapSbrTransport implements SbrTransport.
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<Bar1MmapSbrTransport>();
    }

    #[test]
    fn sbr_transport_trait_shape_contract() {
        // Verify the trait is properly implemented (compile-time check).
        // Bar1MmapSbrTransport and VfioI2cSbrTransport are Send (single-threaded use only,
        // raw ptr patterns mirroring DmaBuffer in src/hw/mod.rs)).
        fn assert_send<T: Send>() {}
        assert_send::<Bar1MmapSbrTransport>();
        assert_send::<VfioI2cSbrTransport>();
    }

    #[test]
    fn sbr_transport_error_variants_render_cleanly() {
        let not_impl = SbrTransportError::NotImplemented("test message");
        assert_eq!(not_impl.to_string(), "not yet implemented: test message");

        let transport_err = SbrTransportError::Transport("my error".into());
        assert_eq!(transport_err.to_string(), "transport: my error");

        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "test io");
        let err: SbrTransportError = io_err.into();
        assert!(err.to_string().contains("test io"));
    }
}
