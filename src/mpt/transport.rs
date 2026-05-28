//! `MptTransport` — abstraction over "how do MPI message bytes flow to/from
//! the Fusion-MPT chip".
//!
//! Two impls live (or will live) under this trait:
//!
//! - **`Mpt3CtlTransport`** (`mpt3ctl.rs`) — kernel-mediated. Wraps the
//!   `/dev/mpt3ctl` character device exposed by Linux's `mpt3sas` driver.
//!   The driver handles all the post-queue plumbing (SRFQ, RPQ, DMA pool,
//!   completion polling). Used for **read-safe operations** (detect, backup,
//!   sbr-read) — the card stays bound to `mpt3sas` so `/dev/sdb` and any
//!   SCSI initiator stay live.
//!
//! - **`VfioDoorbellTransport`** (in `crate::hw::vfio` or a new sibling) —
//!   self-driven. Wraps our own VFIO bind dance + BAR1 mmap + doorbell
//!   handshake. Used for **destructive operations** (flash, recover,
//!   sbr-write) where full chip isolation matters (SCSI is quiesced per
//!   ADR-015 Rule 3; we own every byte; `BindGuard` ensures driver
//!   auto-restore on Drop).
//!
//! See ADR-017 (lsi-flash-notes/01-architecture/adr/017-...) for the
//! selection policy and pluggability principle.

use thiserror::Error;

/// Errors returnable by any `MptTransport` impl.
#[derive(Debug, Error)]
pub enum TransportError {
    /// The transport's backing resource (e.g. `/dev/mpt3ctl`, VFIO group)
    /// could not be located. Usually means the relevant kernel driver isn't
    /// loaded, or no IOC currently manages the requested BDF.
    #[error("transport resource not found: {0}")]
    NotFound(String),

    /// Generic I/O failure from the underlying transport (ioctl error,
    /// read/write failure, etc.).
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The kernel (or chip) actively rejected the request. `errno` is the
    /// raw OS error; `msg` is a human-readable context string.
    #[error("kernel rejected request: errno={errno} ({msg})")]
    KernelReject { errno: i32, msg: String },

    /// Caller's reply buffer was smaller than the bytes the chip wrote.
    /// Indicates a protocol-level sizing mismatch.
    #[error("reply buffer too small: chip wrote {wrote}, buffer is {capacity}")]
    ReplyTooSmall { wrote: usize, capacity: usize },

    /// Catch-all for transport-specific failure modes that don't fit the
    /// other variants. Should be rare in well-behaved impls.
    #[error("transport: {0}")]
    Other(String),
}

/// Abstraction over the MPI message transport. Implementations decide how
/// the request bytes physically reach the chip and how the reply comes
/// back — kernel ioctl, VFIO doorbell, future user-space post-queue, etc.
///
/// Callers (typically `MptCard` or `RealIoc` once refactored) hand in a
/// fully-serialized MPI request frame and receive the reply frame + any
/// bulk data the chip DMA'd.
///
/// Lifetime contract: the transport owns whatever resources it needs (open
/// file descriptors, mmaped regions, bound drivers). Drop releases them
/// cleanly; auto-restore (e.g. `VfioDoorbellTransport` restoring the
/// original driver) happens during Drop.
pub trait MptTransport: Send {
    /// Send an MPI request and receive the reply.
    ///
    /// # Arguments
    ///
    /// - `request`: serialized MPI request frame (header + body). For
    ///   requests with a data SGE (`FW_UPLOAD`, `FW_DOWNLOAD`), the caller
    ///   does **not** include the SGE — the transport handles SGE insertion
    ///   based on the `data_in` / `data_out` slices.
    /// - `reply`: caller-owned buffer for the MPI reply frame. The transport
    ///   writes the reply bytes here and returns how many it wrote.
    /// - `data_in`: optional caller-owned buffer for bulk **IOC→host** data
    ///   (e.g., `FW_UPLOAD` payload). The transport ensures the chip can
    ///   DMA into this buffer (via kernel-allocated bounce pages, IOMMU
    ///   mapping, etc., depending on impl) and copies the result back here.
    /// - `data_out`: optional caller-owned slice for bulk **host→IOC** data
    ///   (e.g., `FW_DOWNLOAD` payload). Transport handles the DMA path.
    ///
    /// # Returns
    ///
    /// `Ok(bytes_written_to_reply)` on success.
    /// `Err(TransportError)` if the transport failed at any layer (resource
    /// missing, ioctl error, kernel rejection, sizing mismatch).
    ///
    /// # Note on SGE offset
    ///
    /// The MPI request format reserves a slot for the SGE at a per-request-
    /// type offset. Caller MUST provide it via `send_with_sge_offset`.
    /// `send` is a convenience wrapper that defaults to offset 5 (the
    /// SGE position for an MPI 2.5+ FW_UPLOAD with no TCSGE).
    ///
    /// MPI 2.0 `FW_UPLOAD` requires a 16-byte TCSGE between the 20-byte
    /// header and the SGE, so its offset is 9 (= 36 bytes / 4).
    fn send(
        &mut self,
        request: &[u8],
        reply: &mut [u8],
        data_in: Option<&mut [u8]>,
        data_out: Option<&mut [u8]>,
    ) -> Result<usize, TransportError> {
        self.send_with_sge_offset(request, 5, reply, data_in, data_out)
    }

    /// Like `send`, but the caller specifies where in the request (in
    /// u32 words from byte 0) the kernel/transport should insert the
    /// data SGE.
    fn send_with_sge_offset(
        &mut self,
        request: &[u8],
        data_sge_offset_words: u32,
        reply: &mut [u8],
        data_in: Option<&mut [u8]>,
        data_out: Option<&mut [u8]>,
    ) -> Result<usize, TransportError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify each TransportError variant renders without template leakage.
    /// (Catches the `"{0}"` typo class where a format placeholder is wrong.)
    #[test]
    fn transport_error_variants_render_cleanly() {
        let cases: Vec<(TransportError, &str)> = vec![
            (
                TransportError::NotFound("/dev/mpt3ctl".into()),
                "transport resource not found: /dev/mpt3ctl",
            ),
            (
                TransportError::KernelReject {
                    errno: 22,
                    msg: "invalid argument".into(),
                },
                "kernel rejected request: errno=22 (invalid argument)",
            ),
            (
                TransportError::ReplyTooSmall {
                    wrote: 200,
                    capacity: 64,
                },
                "reply buffer too small: chip wrote 200, buffer is 64",
            ),
            (TransportError::Other("test".into()), "transport: test"),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }

    /// MptTransport must be Send so a transport can be sent across threads
    /// (e.g. a future where backup runs in a worker thread). Compile-time check.
    #[test]
    fn transport_is_send() {
        fn assert_send<T: Send + ?Sized>() {}
        assert_send::<dyn MptTransport>();
    }
}
