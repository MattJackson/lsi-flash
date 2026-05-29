//! `SbrTransport` — pluggable abstraction over "how do I read the 256-byte
//! SBR EEPROM from the chip". Per ADR-017's pluggability principle, the same
//! shape as MptTransport: trait + multiple impls + caller picks.
//!
//! Two impls today:
//!
//! - **`VfioI2cSbrTransport`** — works on dev-1 silicon. Uses VFIO bind dance
//!   to evict mpt3sas → bind vfio-pci → mmap BAR1 → I²C bit-bang via existing
//!   `src/sbr/i2c.rs` (port of lsirec) → restore mpt3sas on Drop. Cost:
//!   /dev/sdb evicts during the operation, restored after.
//!
//! - **`IstwiSbrTransport`** — proves out via MPI TOOLBOX_ISTWI through
//!   `MptTransport` (mpt3sas stays bound, /dev/sdb stays mounted — no
//!   disruption). Currently returns `NotImplemented` because the
//!   DevIndex/Action/TxData combo for SAS2008 SBR returns IOCStatus
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

    /// Short name for logging ("vfio-i2c" / "istwi" / etc.). Lets callers
    /// log which path was used without matching on concrete types.
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
        // TODO(istwi): chip returns IOCStatus 0x8004 (INTERNAL_ERROR) with
        // DevIndex=0, Action=READ_DATA(0x01), TxDataLength=0, RxDataLength=256.
        Err(SbrTransportError::NotImplemented(
            "istwi: SAS2008 DevIndex/Action combo unsolved; chip returns INTERNAL_ERROR",
        ))

        /* Wire-format research from src/card/mpt.rs::sbr_read (commit 8fd35ff): */
    }

    fn name(&self) -> &'static str {
        "istwi"
    }
}

/// VFIO + BAR1 I²C bit-bang path. Works on dev-1; requires driver flip-flop.
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
        use crate::sbr::i2c::{i2c_read_sbr, I2cContext};

        // Use live BAR1 slice directly (no copy).
        let bar1_slice = self.vfio.bar1();
        let mut ctx = I2cContext {
            bar1: bar1_slice,
            sbr_addr: 0x50,
            eep_type: 0,
        };

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
        Ok(arr)
    }

    fn name(&self) -> &'static str {
        "vfio-i2c"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockMptTransport;

    impl crate::mpt::MptTransport for MockMptTransport {
        fn send_with_sge_offset(
            &mut self,
            _request: &[u8],
            _data_sge_offset_words: u32,
            _reply_buf: &mut [u8],
            _data_in: Option<&mut [u8]>,
            _data_out: Option<&mut [u8]>,
        ) -> Result<usize, crate::mpt::TransportError> {
            panic!(
                "MockMptTransport.send_with_sge_offset called — this should not happen when IstwiSbrTransport is gated behind NotImplemented"
            );
        }
    }

    #[test]
    fn istwi_sbr_transport_returns_not_implemented() {
        let mut transport = IstwiSbrTransport {
            transport: Box::new(MockMptTransport),
        };

        let result = transport.read_sbr();
        match result {
            Err(SbrTransportError::NotImplemented(msg)) => {
                assert!(msg.contains("INTERNAL_ERROR"));
            }
            Ok(_) => panic!("Expected NotImplemented error, got Ok"),
            Err(e) => panic!("Expected NotImplemented error variant, got {:?}", e),
        }
    }

    #[test]
    fn sbr_transport_name_returns_expected_strings() {
        let istwi = IstwiSbrTransport {
            transport: Box::new(MockMptTransport),
        };
        assert_eq!(istwi.name(), "istwi");
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

    #[test]
    fn sbr_transport_trait_shape_contract() {
        let mut istwi_impl: Box<dyn SbrTransport> = Box::new(IstwiSbrTransport {
            transport: Box::new(MockMptTransport),
        });
        assert_eq!(istwi_impl.name(), "istwi");
        let result = istwi_impl.read_sbr();
        assert!(matches!(result, Err(SbrTransportError::NotImplemented(_))));

        fn assert_send<T: Send>() {}
        assert_send::<IstwiSbrTransport>();
        assert_send::<VfioI2cSbrTransport>();
    }
}
