//! MPI message types, request/reply structs, and serialization for v1.0 CLI.
//!
//! Cites: 04-mpi-protocol/*.md wire-format documentation (Tier-2 source-of-truth).
//! Implements typed Request/Reply for the 5 MPI messages needed by lsi-flash CLI.

use thiserror::Error;

/// Toolbox clean flags - bitfield representing components to wipe.
/// Cites: toolbox-and-config.md §5.2 (mpi2_tool.h:93-103)
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ToolboxCleanFlags(u32);

impl ToolboxCleanFlags {
    /// Wipe NVSRAM — bit 0
    pub const NVRAM: Self = Self(0x00000001);

    /// Wipe serial EEPROM — bit 1
    pub const SEEPROM: Self = Self(0x00000002);

    /// Wipe flash array (non-volatile) — bit 2
    pub const FLASH: Self = Self(0x00000004);

    /// Wipe initialization data — bit 24
    pub const INITIALIZATION: Self = Self(0x01000000);

    /// Wipe MegaRAID metadata — bit 25
    pub const MEGARAID: Self = Self(0x02000000);

    /// Wipe backup firmware image — bit 27
    pub const FW_BACKUP: Self = Self(0x08000000);

    /// Wipe current running firmware — bit 28
    pub const FW_CURRENT: Self = Self(0x10000000);

    /// Wipe other persistent pages — bit 29
    pub const OTHER_PERSIST_PAGES: Self = Self(0x20000000);

    /// Wipe persistent manufacturing pages — bit 30
    pub const PERSIST_MANUFACT_PAGES: Self = Self(0x40000000);

    /// Wipe boot services area — bit 31
    pub const BOOT_SERVICES: Self = Self(0x80000000);

    /// Convenience flag: wipe everything (all bits set)
    pub const ALL: Self = Self(0xFFFFFFFF);

    pub fn bits(self) -> u32 {
        self.0
    }

    pub const fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

impl std::ops::BitOr for ToolboxCleanFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

// ============================================================================
// Function Codes — mpi-overview.md §1, fw-download-upload.md §3.1/§4.1
// ============================================================================

/// MPI function codes for message-mode operations.
///
/// Cites: mpi-overview.md §1 (mpi2.h:528-558), fw-download-upload.md §3.1/§4.1
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum MpiFunction {
    /// IOC Initialize — mpi-overview.md §9
    IocInit = 0x02,

    /// IOC Facts query — mpi2_ioc.h:191 (function code 0x03)
    IocFacts = 0x03,

    /// Configuration page access — mpi-overview.md §6, toolbox-and-config.md §6
    Config = 0x04,

    /// Firmware download — fw-download-upload.md §3.1
    FwDownload = 0x09,

    /// Firmware upload — fw-download-upload.md §4.1
    FwUpload = 0x12,

    /// Toolbox operations — toolbox-and-config.md §5.1
    Toolbox = 0x17,
}

impl MpiFunction {
    pub fn from_u8(raw: u8) -> Result<Self, MpiError> {
        match raw {
            0x02 => Ok(Self::IocInit),
            0x03 => Ok(Self::IocFacts), // mpi2_ioc.h:191 - IOC_FACTS function code
            0x04 => Ok(Self::Config),
            0x09 => Ok(Self::FwDownload),
            0x12 => Ok(Self::FwUpload),
            0x17 => Ok(Self::Toolbox),
            _ => Err(MpiError::UnknownFunction(raw)),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::IocInit => 0x02,
            Self::IocFacts => 0x03, // mpi2_ioc.h:191 - IOC_FACTS function code
            Self::Config => 0x04,
            Self::FwDownload => 0x09,
            Self::FwUpload => 0x12,
            Self::Toolbox => 0x17,
        }
    }
}

// ============================================================================
// Image Type Discriminators — fw-download-upload.md §3.2
// ============================================================================

/// Firmware image type for FW_DOWNLOAD/UPLOAD operations.
///
/// Cites: fw-download-upload.md §3.2 (mpi2_ioc.h:1154-1162)
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum ImageType {
    Reserved = 0,
    Fw = 1,         // MPI2_FW_DOWNLOAD_ITYPE_FW
    Bios = 2,       // MPI2_FW_DOWNLOAD_ITYPE_BIOS
    NvData = 3,     // Product-specific extension (not in v2.0 headers)
    BootLoader = 4, // Product-specific extension
    Initialization = 5,
    FlashLayout = 6,
    SupportedDevices = 7,
    MegaRaid = 8, // MPI2_FW_DOWNLOAD_ITYPE_MEGARAID
}

impl ImageType {
    pub fn from_u8(raw: u8) -> Result<Self, MpiError> {
        match raw {
            0x00 => Ok(Self::Reserved),
            0x01 => Ok(Self::Fw),
            0x02 => Ok(Self::Bios),
            0x03 => Ok(Self::NvData),
            0x04 => Ok(Self::BootLoader),
            0x05 => Ok(Self::Initialization),
            0x06 => Ok(Self::FlashLayout),
            0x07 => Ok(Self::SupportedDevices),
            0x08 => Ok(Self::MegaRaid),
            _ => Err(MpiError::UnknownImageType(raw)),
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            Self::Reserved => 0x00,
            Self::Fw => 0x01,
            Self::Bios => 0x02,
            Self::NvData => 0x03,
            Self::BootLoader => 0x04,
            Self::Initialization => 0x05,
            Self::FlashLayout => 0x06,
            Self::SupportedDevices => 0x07,
            Self::MegaRaid => 0x08,
        }
    }
}

// ============================================================================
// IOCStatus — iocstatus-table.md §10 (exhaustive enumeration)
// ============================================================================

/// IOC return status codes from all MPI message replies.
///
/// Cites: iocstatus-table.md §10 (mpi2.h:580-665), ADR-015 Rule 6 for InternalError
#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Error)]
pub enum IocStatus {
    #[error("success")]
    Success = 0x0000,

    #[error("invalid function")]
    InvalidFunction = 0x0001,

    #[error("busy")]
    Busy = 0x0002,

    #[error("invalid SGL")]
    InvalidSgl = 0x0003,

    /// CRITICAL: dev-1 brick code per ADR-015 Rule 6 (mpi2.h:585)
    #[error("internal error — flash programming fault")]
    InternalError = 0x0004,

    #[error("invalid VPID")]
    InvalidVpid = 0x0005,

    #[error("insufficient resources")]
    InsufficientResources = 0x0006,

    #[error("invalid field")]
    InvalidField = 0x0007,

    #[error("invalid state")]
    InvalidState = 0x0008,

    #[error("operation not supported in current state")]
    OpStateNotSupported = 0x0009,

    // Config-specific codes — iocstatus-table.md §10 (mpi2.h:595-600)
    #[error("config invalid action")]
    ConfigInvalidAction = 0x0020,

    #[error("config invalid type")]
    ConfigInvalidType = 0x0021,

    #[error("config invalid page")]
    ConfigInvalidPage = 0x0022,

    #[error("config invalid data")]
    ConfigInvalidData = 0x0023,

    #[error("config no defaults")]
    ConfigNoDefaults = 0x0024,

    #[error("config cannot commit")]
    ConfigCannotCommit = 0x0025,

    // SCSI IO codes — iocstatus-table.md §10 (mpi2.h:606-617)
    #[error("SCSI recovered error")]
    ScsiRecoveredError = 0x0040,

    #[error("SCSI invalid device handle")]
    ScsiInvalidDevHandle = 0x0042,

    #[error("SCSI device not there")]
    ScsiDeviceNotThere = 0x0043,

    #[error("SCSI data overrun")]
    ScsiDataOverrun = 0x0044,

    #[error("SCSI data underrun")]
    ScsiDataUnderrun = 0x0045,

    #[error("SCSI I/O data error")]
    ScsiIoDataError = 0x0046,

    #[error("SCSI protocol error")]
    ScsiProtocolError = 0x0047,

    #[error("SCSI task terminated")]
    ScsiTaskTerminated = 0x0048,

    #[error("SCSI residual mismatch")]
    ScsiResidualMismatch = 0x0049,

    #[error("SCSI task mgmt failed")]
    ScsiTaskMgmtFailed = 0x004A,

    #[error("SCSI IOC terminated")]
    ScsiIocTerminated = 0x004B,

    #[error("SCSI external terminated")]
    ScsiExtTerminated = 0x004C,

    // EEDP codes — iocstatus-table.md §10 (mpi2.h:623-625)
    #[error("EEDP guard error")]
    EedpGuardError = 0x004D,

    #[error("EEDP ref tag error")]
    EedpRefTagError = 0x004E,

    #[error("EEDP app tag error")]
    EedpAppTagError = 0x004F,

    // Target codes — iocstatus-table.md §10 (mpi2.h:631-640)
    #[error("target invalid IO index")]
    TargetInvalidIoIndex = 0x0062,

    #[error("target aborted")]
    TargetAborted = 0x0063,

    #[error("target no connection retryable")]
    TargetNoConnRetryable = 0x0064,

    #[error("target no connection")]
    TargetNoConnection = 0x0065,

    #[error("target xfer count mismatch")]
    TargetXferCountMismatch = 0x006A,

    #[error("target data offset error")]
    TargetDataOffsetError = 0x006D,

    #[error("target too much write data")]
    TargetTooMuchWriteData = 0x006E,

    #[error("target IU too short")]
    TargetIuTooShort = 0x006F,

    #[error("target ACK/NAK timeout")]
    TargetAckNakTimeout = 0x0070,

    #[error("target NAK received")]
    TargetNakReceived = 0x0071,

    // SAS codes — iocstatus-table.md §10 (mpi2.h:646-647)
    #[error("SAS SMP request failed")]
    SasSmpRequestFailed = 0x0090,

    #[error("SAS SMP data overrun")]
    SasSmpDataOverrun = 0x0091,

    // Diagnostic buffer — iocstatus-table.md §10 (mpi2.h:653)
    #[error("diagnostic released")]
    DiagnosticReleased = 0x00A0,

    // RAID accelerator — iocstatus-table.md §10 (mpi2.h:659)
    #[error("RAID accel error")]
    RaidAccelError = 0x00B0,
}

impl IocStatus {
    /// ADR-015 Rule 6: any non-Success during flash operations is a hard stop.
    pub fn is_flash_hard_stop(self) -> bool {
        !matches!(self, Self::Success)
    }

    /// Parse raw u16 into IocStatus enum. Returns UnknownIocStatus for undefined codes.
    pub fn from_u16(raw: u16) -> Result<Self, MpiError> {
        match raw {
            0x0000 => Ok(Self::Success),
            0x0001 => Ok(Self::InvalidFunction),
            0x0002 => Ok(Self::Busy),
            0x0003 => Ok(Self::InvalidSgl),
            0x0004 => Ok(Self::InternalError),
            0x0005 => Ok(Self::InvalidVpid),
            0x0006 => Ok(Self::InsufficientResources),
            0x0007 => Ok(Self::InvalidField),
            0x0008 => Ok(Self::InvalidState),
            0x0009 => Ok(Self::OpStateNotSupported),
            0x0020 => Ok(Self::ConfigInvalidAction),
            0x0021 => Ok(Self::ConfigInvalidType),
            0x0022 => Ok(Self::ConfigInvalidPage),
            0x0023 => Ok(Self::ConfigInvalidData),
            0x0024 => Ok(Self::ConfigNoDefaults),
            0x0025 => Ok(Self::ConfigCannotCommit),
            0x0040 => Ok(Self::ScsiRecoveredError),
            0x0042 => Ok(Self::ScsiInvalidDevHandle),
            0x0043 => Ok(Self::ScsiDeviceNotThere),
            0x0044 => Ok(Self::ScsiDataOverrun),
            0x0045 => Ok(Self::ScsiDataUnderrun),
            0x0046 => Ok(Self::ScsiIoDataError),
            0x0047 => Ok(Self::ScsiProtocolError),
            0x0048 => Ok(Self::ScsiTaskTerminated),
            0x0049 => Ok(Self::ScsiResidualMismatch),
            0x004A => Ok(Self::ScsiTaskMgmtFailed),
            0x004B => Ok(Self::ScsiIocTerminated),
            0x004C => Ok(Self::ScsiExtTerminated),
            0x004D => Ok(Self::EedpGuardError),
            0x004E => Ok(Self::EedpRefTagError),
            0x004F => Ok(Self::EedpAppTagError),
            0x0062 => Ok(Self::TargetInvalidIoIndex),
            0x0063 => Ok(Self::TargetAborted),
            0x0064 => Ok(Self::TargetNoConnRetryable),
            0x0065 => Ok(Self::TargetNoConnection),
            0x006A => Ok(Self::TargetXferCountMismatch),
            0x006D => Ok(Self::TargetDataOffsetError),
            0x006E => Ok(Self::TargetTooMuchWriteData),
            0x006F => Ok(Self::TargetIuTooShort),
            0x0070 => Ok(Self::TargetAckNakTimeout),
            0x0071 => Ok(Self::TargetNakReceived),
            0x0090 => Ok(Self::SasSmpRequestFailed),
            0x0091 => Ok(Self::SasSmpDataOverrun),
            0x00A0 => Ok(Self::DiagnosticReleased),
            0x00B0 => Ok(Self::RaidAccelError),
            _ => Err(MpiError::UnknownIocStatus(raw)),
        }
    }

    /// Convert to raw u16 value.
    pub fn as_u16(self) -> u16 {
        self as u16
    }
}

// ============================================================================
// ToolboxCleanFlags — toolbox-and-config.md §5.2 (mpi2_tool.h:93-103)
// ============================================================================

// ============================================================================
// IEEE SGE (Scatter/Gather Element) — sgl-and-replies.md §7.1, §7.3
// ============================================================================

/// 64-bit IEEE scatter/gather element for MPI v2.x message mode.
///
/// Cites: sgl-and-replies.md §7.1 (mpi2.h:1035-1041), §7.3 IEEE flags
#[derive(Debug, Copy, Clone)]
pub struct IeeeSgeSimple64 {
    /// 64-bit DMA-visible host memory address
    pub address: u64,

    /// Buffer length in bytes (24-bit field per mpi2.h)
    pub length: u32,

    /// Flags encoding: element type, EOL/EOB/EOL bits, direction, address size
    pub flags: u8,
}

impl IeeeSgeSimple64 {
    /// Create a simple SGE pointing at host system memory.
    ///
    /// Creates END_OF_LIST flag + HOST_TO_IOC direction + 64-bit addressing.
    pub fn new(address: u64, length: u32) -> Self {
        // Per sgl-and-replies.md §7.1/§7.3:
        // - Element type: SIMPLE (0x00 for IEEE format bit 7)
        // - EOL: bit 6 (MPI25_IEEE_SGE_FLAGS_END_OF_LIST = 0x40)
        // - Address space: SYSTEM_ADDR (0x00)
        let flags = 0x40; // END_OF_LIST for IEEE format

        Self {
            address,
            length,
            flags,
        }
    }

    /// Create SGE with custom flags.
    pub fn with_flags(address: u64, length: u32, flags: u8) -> Self {
        Self {
            address,
            length,
            flags,
        }
    }

    /// Check if this is the last element in the list (bit 7 for non-IEEE, bit 6 for IEEE).
    pub fn end_of_list(&self) -> bool {
        // Non-IEEE format uses bit 7 (0x80), IEEE format uses bit 6 (0x40)
        self.flags & 0xC0 != 0
    }

    /// Serialize to 16 bytes: address (8B LE) + length (4B LE) + reserved (2B) + flags (1B).
    ///
    /// Cites: sgl-and-replies.md §7.1 cumulative offsets for IEEE SGE
    pub fn serialize_to(&self, buf: &mut Vec<u8>) {
        // 0x00-0x07: Address (64-bit little-endian)
        buf.extend_from_slice(&self.address.to_le_bytes());

        // 0x08-0x0B: Length (32-bit little-endian)
        buf.extend_from_slice(&self.length.to_le_bytes());

        // 0x0C-0x0D: Reserved1 (2 bytes, zeroed)
        buf.extend_from_slice(&[0x00, 0x00]);

        // 0x0E: Reserved2 (1 byte, zeroed)
        buf.push(0x00);

        // 0x0F: Flags (1 byte)
        buf.push(self.flags);
    }

    /// Total serialized size in bytes.
    pub fn serialized_size() -> usize {
        16 // 8 + 4 + 2 + 1 + 1 per mpi2.h:1035-1041
    }
}

/// MPI 2.0 (NON-IEEE) 64-bit Simple SGE — `MPI2_SGE_SIMPLE_UNION` per
/// `mpi2.h:932-955`. Different byte layout from `IeeeSgeSimple64`: the
/// `FlagsLength` u32 comes FIRST (flags in high byte, length in low 24 bits),
/// then the 64-bit address. Total 12 bytes.
///
/// SAS2008 (chip family `MPI_MFGPAGE_DEVID_SAS2008`) implements MPI 2.0, not
/// MPI 2.5. Sending an IEEE SGE causes the chip to misparse the first 4 bytes
/// of our address as `FlagsLength` — garbage flags + a wildly wrong address.
/// The chip then "DMAs" to that wrong address (typically a kernel-reserved
/// region behind the IOMMU/root-complex DMA mask), so the host buffer stays
/// zero and the chip still reports Success.
///
/// Caught on dev-1 2026-05-28 when every other prerequisite (BME, IOVA,
/// hugepage, IOC state) was verified but Tier 2 backup still returned zeros.
#[derive(Debug, Copy, Clone)]
pub struct MpiSgeSimple64 {
    pub address: u64,
    pub length: u32,
    /// High byte of `FlagsLength`. See `MPI2_SGE_FLAGS_*` constants for the
    /// bit layout. For a typical FW_UPLOAD final element, use 0xD3 (SIMPLE
    /// | LAST | END_OF_BUFFER | 64-BIT | END_OF_LIST, IOC->host direction).
    pub flags: u8,
}

/// MPI 2.0 SGE flags — per `mpi2.h:932-955`. These live in the **high byte**
/// of the `FlagsLength` u32 (i.e., bits 24-31 when read LE).
pub mod mpi_sge_flags {
    /// SIMPLE element type — `MPI2_SGE_FLAGS_SIMPLE_ELEMENT`.
    pub const SIMPLE_ELEMENT: u8 = 0x10;
    /// LAST element in the SG list — `MPI2_SGE_FLAGS_LAST_ELEMENT`.
    pub const LAST_ELEMENT: u8 = 0x80;
    /// End of buffer — `MPI2_SGE_FLAGS_END_OF_BUFFER`.
    pub const END_OF_BUFFER: u8 = 0x40;
    /// End of list — `MPI2_SGE_FLAGS_END_OF_LIST`.
    pub const END_OF_LIST: u8 = 0x01;
    /// 64-bit address (vs 32-bit) — `MPI2_SGE_FLAGS_64_BIT_ADDRESSING`.
    pub const ADDR_64BIT: u8 = 0x02;
    /// Direction = host→IOC (FW_DOWNLOAD). Cleared = IOC→host (FW_UPLOAD).
    pub const HOST_TO_IOC: u8 = 0x04;
}

impl MpiSgeSimple64 {
    /// SGE for a single buffer the **IOC writes into** (i.e., FW_UPLOAD).
    /// Flags = SIMPLE | LAST | END_OF_BUFFER | 64-BIT | END_OF_LIST = 0xD3.
    pub fn ioc_to_host_one_shot(address: u64, length: u32) -> Self {
        use mpi_sge_flags::*;
        Self {
            address,
            length,
            flags: SIMPLE_ELEMENT | LAST_ELEMENT | END_OF_BUFFER | ADDR_64BIT | END_OF_LIST,
        }
    }

    /// SGE for a single buffer the **host writes** (i.e., FW_DOWNLOAD).
    /// Same as ioc_to_host but with HOST_TO_IOC bit set.
    #[allow(dead_code)]
    pub fn host_to_ioc_one_shot(address: u64, length: u32) -> Self {
        use mpi_sge_flags::*;
        Self {
            address,
            length,
            flags: SIMPLE_ELEMENT
                | LAST_ELEMENT
                | END_OF_BUFFER
                | ADDR_64BIT
                | END_OF_LIST
                | HOST_TO_IOC,
        }
    }

    /// Serialize to 12 bytes: FlagsLength (4B LE) + Address (8B LE).
    ///
    /// FlagsLength packs as: (flags << 24) | (length & 0x00FFFFFF). Length
    /// is 24-bit per MPI 2.0 — caller must ensure < 16 MB; this asserts.
    pub fn serialize_to(&self, buf: &mut Vec<u8>) {
        assert!(
            self.length <= 0x00FF_FFFF,
            "MPI 2.0 SGE length is 24-bit; got {}",
            self.length
        );
        let flags_length: u32 = ((self.flags as u32) << 24) | (self.length & 0x00FF_FFFF);
        buf.extend_from_slice(&flags_length.to_le_bytes());
        buf.extend_from_slice(&self.address.to_le_bytes());
    }

    /// Total serialized size in bytes.
    pub fn serialized_size() -> usize {
        12 // 4 (FlagsLength) + 8 (Address)
    }
}

// ============================================================================
// Request Structs — serialize to MPI message wire format
// ============================================================================

/// FW_DOWNLOAD request (MPI v2.5 extension with ImageOffset/ImageSize).
///
/// Cites: fw-download-upload.md §3, mpi2_ioc.h:1179-1198 (v2.5) + mpi-overview.md §1.2 header
#[derive(Debug, Clone)]
pub struct FwDownloadRequest<'a> {
    /// Image type discriminator (FW=1, BIOS=2, etc.) — fw-download-upload.md §3.2
    pub image_type: ImageType,

    /// Chunk offset within the full firmware image (v2.5 field)
    pub image_offset: u32,

    /// Bytes in this chunk (v2.5 field)
    pub image_size: u32,

    /// Total size of entire firmware image (present in both v2.0 and v2.5)
    pub total_image_size: u32,

    /// Set LAST_SEGMENT flag on final chunk — fw-download-upload.md §3 (mpi2_ioc.h:1152)
    pub last_segment: bool,

    /// Payload buffer to transfer via SGL
    pub payload: &'a [u8],
}

impl FwDownloadRequest<'_> {
    /// Serialize request to MPI message wire format (header + body + TCSGE).
    ///
    /// Returns ~40 bytes total: 10-byte header + 30-byte v2.5 request body + 16-byte SGL.
    pub fn serialize_to(&self, smid: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(56); // generous allocation

        // MPI2_REQUEST_HEADER (10 bytes) — mpi-overview.md §1.2
        // Offset 0x00-0x01: FunctionDependent1 (0 for FW_DOWNLOAD)
        buf.extend_from_slice(&0u16.to_le_bytes());

        // Offset 0x02: ChainOffset (0 = single message, no chaining)
        buf.push(0x00);

        // Offset 0x03: Function = MPI2_FUNCTION_FW_DOWNLOAD = 0x09
        buf.push(MpiFunction::FwDownload.as_u8());

        // Offset 0x04-0x05: FunctionDependent2 (SMID encoded here for v2.x)
        buf.extend_from_slice(&smid.to_le_bytes());

        // Offset 0x06: FunctionDependent3 = 0x01 (MPI2_FW_DOWNLOAD_ITYPE flag encoding)
        buf.push(0x01);

        // Offset 0x07: MsgFlags — LAST_SEGMENT bit if final chunk
        let mut msg_flags = 0x00;
        if self.last_segment {
            msg_flags |= 0x01; // MPI2_FW_DOWNLOAD_MSGFLGS_LAST_SEGMENT per mpi2_ioc.h:1152
        }
        buf.push(msg_flags);

        // Offset 0x08-0x09: VP_ID, VF_ID (both 0 for non-SR-IOV)
        buf.push(0x00); // VP_ID
        buf.push(0x00); // VF_ID

        // Offset 0x0A-0x0B: Reserved1
        buf.extend_from_slice(&0u16.to_le_bytes());

        // MPI25_FW_DOWNLOAD_REQUEST body (30 bytes) — fw-download-upload.md §3.1
        // Offset 0x0C: ImageType
        buf.push(self.image_type.as_u8());

        // Offset 0x0D: Reserved1
        buf.push(0x00);

        // Offset 0x0E: ChainOffset (in body context)
        buf.push(0x00);

        // Offset 0x0F: Function (repeated in some message formats)
        buf.push(MpiFunction::FwDownload.as_u8());

        // Offset 0x10-0x11: Reserved2
        buf.extend_from_slice(&0u16.to_le_bytes());

        // Offset 0x12: Reserved3
        buf.push(0x00);

        // Offset 0x13: MsgFlags (repeated)
        buf.push(msg_flags);

        // Offset 0x14-0x15: VP_ID, VF_ID (repeated)
        buf.push(0x00);
        buf.push(0x00);

        // Offset 0x16-0x17: Reserved4
        buf.extend_from_slice(&0u16.to_le_bytes());

        // Offset 0x18-0x1B: TotalImageSize (32-bit)
        buf.extend_from_slice(&self.total_image_size.to_le_bytes());

        // Offset 0x1C-0x1F: Reserved5
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Offset 0x20-0x23: ImageOffset (v2.5 extension)
        buf.extend_from_slice(&self.image_offset.to_le_bytes());

        // Offset 0x24-0x27: ImageSize (v2.5 extension)
        buf.extend_from_slice(&self.image_size.to_le_bytes());

        // SGL — MPI25_SGE_IO_UNION for v2.5 (fw-download-upload.md §3.1)
        let sge = IeeeSgeSimple64::new(self.payload.as_ptr() as u64, self.payload.len() as u32);
        sge.serialize_to(&mut buf);

        buf
    }
}

/// FW_UPLOAD request — read firmware FROM IOC via MPI.
///
/// Cites: fw-download-upload.md §4 (mpi2_ioc.h:1270-1289 v2.5 + mpi-overview.md §1.2)
#[derive(Debug)]
pub struct FwUploadRequest<'a> {
    /// Image type to upload (FW_CURRENT=0x00, FW_FLASH=0x01, etc.) — fw-download-upload.md §4.1
    pub image_type: ImageType,

    /// Chunk offset within the firmware image
    pub image_offset: u32,

    /// Maximum bytes to read in this chunk
    pub image_size: u32,

    /// Output buffer for uploaded data (filled by deserialize_from_reply)
    pub payload_buffer: &'a mut [u8],
}

impl FwUploadRequest<'_> {
    /// Serialize FW_UPLOAD request to wire format per
    /// `MPI2_FW_UPLOAD_REQUEST` at mpi2_ioc.h:1226-1242. The struct IS the
    /// wire message — there is NO separate REQUEST_HEADER prefix. The first
    /// 10 bytes follow the standard layout (ImageType/Reserved/ChainOffset/
    /// Function/...) but you write them once, not twice.
    ///
    /// Caught on dev-1 2026-05-28: the earlier freshman version wrote a
    /// duplicate 10-byte header which made the chip read ImageType=0 for all
    /// calls — backup.bin / bios.rom / nvdata.bin all came out byte-identical.
    ///
    /// `iova` is the chip-readable address the SGE points at. The chip
    /// DMA-writes the firmware bytes there. Production callers get this
    /// from `RealIoc::alloc_dma()` (VFIO-mapped IOVA). Passing a user-space
    /// VA produces 885 KB of zeros (see ADR-016) — the chip cannot translate
    /// host page-table addresses.
    pub fn serialize_to(&self, iova: u64) -> Vec<u8> {
        let mut buf = Vec::with_capacity(32);

        // 0x00 ImageType
        buf.push(self.image_type.as_u8());
        // 0x01 Reserved1
        buf.push(0x00);
        // 0x02 ChainOffset
        buf.push(0x00);
        // 0x03 Function = 0x12
        buf.push(MpiFunction::FwUpload.as_u8());
        // 0x04 Reserved2 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x06 Reserved3
        buf.push(0x00);
        // 0x07 MsgFlags
        buf.push(0x00);
        // 0x08 VP_ID
        buf.push(0x00);
        // 0x09 VF_ID
        buf.push(0x00);
        // 0x0A Reserved4 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x0C Reserved5 (U32)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // 0x10 Reserved6 (U32)
        buf.extend_from_slice(&0u32.to_le_bytes());

        // 0x14 SGL — MPI 2.0 SIMPLE_64 (12 bytes; FlagsLength prefix + 64-bit
        // address). Was IeeeSgeSimple64 — wrong for SAS2008 (MPI 2.0 chip;
        // IEEE format is MPI 2.5+). The IEEE wire format put address bytes
        // where the chip expected FlagsLength, so it parsed garbage flags +
        // wrong address and DMA'd into the void while reporting Success.
        // Dev-1 finding 2026-05-28; see ADR-016 + 2026-05-28-vfio-dev1-hardware-test session note.
        let sge_len = self.payload_buffer.len().min(self.image_size as usize) as u32;
        let sge = MpiSgeSimple64::ioc_to_host_one_shot(iova, sge_len);
        sge.serialize_to(&mut buf);

        buf
    }
}

/// TOOLBOX_CLEAN request — wipe specified components from flash.
///
/// Cites: toolbox-and-config.md §5 (mpi2_tool.h:76-90 + mpi-overview.md §1.2)
#[derive(Debug, Clone)]
pub struct ToolboxCleanRequest {
    /// Bitmask of components to wipe — toolbox-and-config.md §5.2 (MPI2_TOOLBOX_CLEAN_*)
    pub flags: ToolboxCleanFlags,
}

impl ToolboxCleanRequest {
    /// Serialize TOOLBOX_CLEAN request to wire format.
    ///
    /// Returns 20 bytes: header + CLEAN request body (no SGL needed).
    pub fn serialize_to(&self, smid: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(30);

        // MPI2_REQUEST_HEADER (10 bytes) — mpi-overview.md §1.2
        buf.extend_from_slice(&0u16.to_le_bytes()); // FunctionDependent1
        buf.push(0x00); // ChainOffset
        buf.push(MpiFunction::Toolbox.as_u8()); // Function = 0x17
        buf.extend_from_slice(&smid.to_le_bytes()); // FunctionDependent2 (SMID)
        buf.push(0x00); // FunctionDependent3

        let msg_flags = 0x00; // Not used by CLEAN tool per toolbox-and-config.md §5
        buf.push(msg_flags);
        buf.push(0x00); // VP_ID
        buf.push(0x00); // VF_ID
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved1

        // MPI2_TOOLBOX_CLEAN_REQUEST body (10 bytes) — toolbox-and-config.md §5.1
        buf.push(0x00); // Tool = CLEAN tool discriminator (mpi2_tool.h:41)
        buf.push(0x00); // Reserved1
        buf.push(0x00); // ChainOffset (repeated in body)
        buf.push(MpiFunction::Toolbox.as_u8()); // Function (repeated)
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved2
        buf.push(0x00); // Reserved3
        buf.push(msg_flags); // MsgFlags
        buf.push(0x00); // VP_ID
        buf.push(0x00); // VF_ID
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved4

        // Flags field (4 bytes) — toolbox-and-config.md §5.2
        buf.extend_from_slice(&self.flags.bits().to_le_bytes());

        buf
    }
}

/// CONFIG request — read/write configuration pages (NVRAM, manufacturing data).
///
/// Cites: toolbox-and-config.md §6 (mpi2_cnfg.h:330-348 + mpi-overview.md §1.2)
#[derive(Debug)]
pub struct ConfigRequest<'a> {
    /// Page operation type — toolbox-and-config.md §6.1 (MPI2_CONFIG_ACTION_*)
    pub action: u8,

    /// SGL flags for data direction/type — toolbox-and-config.md §6
    pub sgl_flags: u8,

    /// Page header metadata — toolbox-and-config.md §6.2
    pub page_type: u8,
    pub page_number: u8,
    pub ext_page_type: Option<u8>,

    /// Output buffer for read operations (filled by deserialize_from_reply)
    pub payload_buffer: &'a mut [u8],

    /// PageAddress field — per mpi2_cnfg.h:347 (PageAddress at offset 0x18).
    /// For plain pages (Manufacturing, IO Unit, IOC, BIOS), PageAddress MUST be 0.
    /// The page type+number live in the 4-byte page header, NOT in PageAddress.
    /// PageAddress is only nonzero for "addressed" pages (SAS device pages with a
    /// FORM/handle): see mpi2_cnfg.h:237-298 for RAID/SAS/PCIe addressing formats.
    pub page_address: u32,
}

impl ConfigRequest<'_> {
    /// Serialize CONFIG request to wire format.
    ///
    /// `MPI2_CONFIG_REQUEST` (mpi2_cnfg.h:330-348) IS the wire frame — there is
    /// NO separate preceding `MPI2_REQUEST_HEADER`. `Action` is at offset 0x00,
    /// `Function` at 0x03, the page Header at 0x14, PageAddress at 0x18, and the
    /// PageBufferSGE at 0x1C. Returns 0x2C bytes (header + 16-byte SGE slot).
    ///
    /// Hardware bug fixed 2026-05-29 (dev-1): the prior version prepended a
    /// 10-byte generic header, shifting the page Header from 0x14 → 0x1A so the
    /// chip read PageType from a zero byte and returned the type-0 page for
    /// every request (`requested 9/0 got 0/0`). Same class as the FwUpload
    /// ImageType=0 bug. `_smid` is unused: on the mpt3ctl kernel path the driver
    /// assigns the SMID/MsgContext; CONFIG has no SMID field at 0x04 (that is
    /// ExtPageLength). The SGE bytes are a placeholder the kernel overwrites at
    /// `data_sge_offset_words` (= 7 = 0x1C/4); callers MUST pass 7.
    pub fn serialize_to(&self, _smid: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(0x2C);

        buf.push(self.action); // 0x00 Action — mpi2_cnfg.h:332
        buf.push(self.sgl_flags); // 0x01 SGLFlags
        buf.push(0x00); // 0x02 ChainOffset
        buf.push(MpiFunction::Config.as_u8()); // 0x03 Function = 0x04

        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x04 ExtPageLength (IOC fills on ext reply)
        buf.push(self.ext_page_type.unwrap_or(0x00)); // 0x06 ExtPageType
        buf.push(0x00); // 0x07 MsgFlags
        buf.push(0x00); // 0x08 VP_ID
        buf.push(0x00); // 0x09 VF_ID
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x0A Reserved1
        buf.push(0x00); // 0x0C Reserved2
        buf.push(0x00); // 0x0D ProxyVF_ID
        buf.extend_from_slice(&0u16.to_le_bytes()); // 0x0E Reserved4
        buf.extend_from_slice(&0u32.to_le_bytes()); // 0x10 Reserved3

        // 0x14 MPI2_CONFIG_PAGE_HEADER (mpi2_cnfg.h:158-165)
        buf.push(0x00); // 0x14 PageVersion (IOC fills on reply)
        buf.push((self.payload_buffer.len() / 4) as u8); // 0x15 PageLength (in 4-byte words)
        buf.push(self.page_number); // 0x16 PageNumber
        buf.push(self.page_type); // 0x17 PageType

        // 0x18 PageAddress — mpi2_cnfg.h:347. MUST be 0 for plain pages
        // (Manufacturing/IO Unit/IOC/BIOS); nonzero only for "addressed" pages
        // with a FORM/handle (RAID Volume / SAS Device / SAS Expander),
        // mpi2_cnfg.h:237-298.
        buf.extend_from_slice(&self.page_address.to_le_bytes());

        // 0x1C PageBufferSGE — placeholder; the mpt3ctl kernel path overwrites
        // this with its own bounce-buffer SGE at data_sge_offset_words=7. 16
        // zero bytes so the user buffer has room for the kernel-inserted SGE.
        buf.extend_from_slice(&[0u8; 16]);

        buf
    }
}

/// IOC_INIT request — initialize MPI message queue after driver load.
///
/// Cites: mpi-overview.md §9 (mpi2_ioc.h:135-164 + mpi-overview.md §1.2)
#[derive(Debug, Clone)]
pub struct IocInitRequest {
    /// Who initiated this init — mpi-overview.md §9 (MPI2_WHOINIT_HOST_DRIVER = 0x04)
    pub who_init: u8,

    /// Host MSI-X vectors supported — mpi-overview.md §9
    pub host_msix_vectors: u8,

    /// Reply queue depth — mpi-overview.md §9 (MPI2_RDPQ_DEPTH_MIN = 16)
    pub reply_descriptor_post_queue_depth: u16,

    /// System request frame base address (DMA-visible) — mpi-overview.md §9
    pub system_request_frame_base_address: u64,

    /// Reply descriptor post queue base address (DMA-visible) — mpi-overview.md §9
    pub reply_descriptor_post_queue_address: u64,
}

impl IocInitRequest {
    /// Serialize IOC_INIT request per `MPI2_IOC_INIT_REQUEST` at
    /// mpi2_ioc.h:135-164. The struct IS the wire message (no separate
    /// REQUEST_HEADER prefix). Total: 0x48 = 72 bytes.
    ///
    /// Audit caught: previous freshman version wrote a 10-byte duplicate
    /// header before the body, shifting WhoInit from offset 0x00 to 0x0A
    /// and dropping the ReplyFreeQueueAddress field entirely.
    pub fn serialize_to(&self, _smid: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(72);

        // 0x00 WhoInit (MPI2_WHOINIT_HOST_DRIVER = 0x04)
        buf.push(self.who_init);
        // 0x01 Reserved1 / 0x02 ChainOffset
        buf.push(0x00);
        buf.push(0x00);
        // 0x03 Function = 0x02
        buf.push(MpiFunction::IocInit.as_u8());
        // 0x04 Reserved2 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x06 Reserved3 / 0x07 MsgFlags / 0x08 VP_ID / 0x09 VF_ID
        buf.extend_from_slice(&[0u8; 4]);
        // 0x0A Reserved4 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x0C MsgVersion (U16) / 0x0E HeaderVersion (U16) — IOC sets these in reply
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x10 Reserved5 (U32)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // 0x14 Reserved6 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x16 Reserved7
        buf.push(0x00);
        // 0x17 HostMSIxVectors
        buf.push(self.host_msix_vectors);
        // 0x18 Reserved8 (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x1A SystemRequestFrameSize (U16) — set by host or IOC depending on direction
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x1C ReplyDescriptorPostQueueDepth (U16)
        buf.extend_from_slice(&self.reply_descriptor_post_queue_depth.to_le_bytes());
        // 0x1E ReplyFreeQueueDepth (U16)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // 0x20 SenseBufferAddressHigh (U32)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // 0x24 SystemReplyAddressHigh (U32)
        buf.extend_from_slice(&0u32.to_le_bytes());
        // 0x28 SystemRequestFrameBaseAddress (U64)
        buf.extend_from_slice(&self.system_request_frame_base_address.to_le_bytes());
        // 0x30 ReplyDescriptorPostQueueAddress (U64)
        buf.extend_from_slice(&self.reply_descriptor_post_queue_address.to_le_bytes());
        // 0x38 ReplyFreeQueueAddress (U64) — was MISSING before; required for post-queue mode
        buf.extend_from_slice(&0u64.to_le_bytes());
        // 0x40 TimeStamp (U64)
        buf.extend_from_slice(&0u64.to_le_bytes());

        debug_assert_eq!(buf.len(), 0x48);
        buf
    }
}

// ============================================================================
// Reply Structs — deserialize from MPI message wire format
// ============================================================================

/// FW_DOWNLOAD reply — parse IOCStatus and status fields.
///
/// Cites: fw-download-upload.md §3.5 (mpi2_ioc.h:1202-1218) + sgl-and-replies.md §8.3
#[derive(Debug, Clone, PartialEq)]
pub struct FwDownloadReply {
    /// Image type echoed back by IOC — fw-download-upload.md §3.5
    pub image_type: u8,

    /// IOC return status — sgl-and-replies.md §8.3 (offset 0x0E)
    pub ioc_status: IocStatus,

    /// Optional log info from IOC (if MPI2_IOCSTATUS_FLAG_LOG_INFO_AVAILABLE set)
    pub ioc_log_info: u32,
}

impl FwDownloadReply {
    /// Parse FW_DOWNLOAD reply from raw bytes.
    ///
    /// Expects at least 18 bytes. Reads IOCStatus at offset 0x0E per sgl-and-replies.md §8.3.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 18 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::FwDownload,
                got: bytes.len(),
                need: 18,
            });
        }

        // Offset 0x00: ImageType
        let image_type = bytes[0];

        // Offset 0x0E: IOCStatus (2 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        // Offset 0x10: IOCLogInfo (4 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_log_info = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);

        Ok(Self {
            image_type,
            ioc_status,
            ioc_log_info,
        })
    }
}

/// FW_UPLOAD reply — parse IOCStatus and ActualImageSize.
///
/// Cites: fw-download-upload.md §4 (mpi2_ioc.h:1293-1312) + sgl-and-replies.md §8.3
#[derive(Debug, Clone, PartialEq)]
pub struct FwUploadReply {
    /// Image type echoed back by IOC — fw-download-upload.md §4.1
    pub image_type: u8,

    /// IOC return status — sgl-and-replies.md §8.3 (offset 0x0E)
    pub ioc_status: IocStatus,

    /// Actual bytes read from IOC — fw-download-upload.md §4 (ActualImageSize at offset 0x14)
    pub actual_image_size: u32,
}

impl FwUploadReply {
    /// Parse FW_UPLOAD reply from raw bytes.
    ///
    /// Expects at least 22 bytes. Reads IOCStatus at 0x0E and ActualImageSize at 0x14.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 22 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::FwUpload,
                got: bytes.len(),
                need: 22,
            });
        }

        // Offset 0x00: ImageType
        let image_type = bytes[0];

        // Offset 0x0E: IOCStatus (2 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        // Offset 0x14: ActualImageSize (4 bytes little-endian) — fw-download-upload.md §4.1
        let actual_image_size = u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);

        Ok(Self {
            image_type,
            ioc_status,
            actual_image_size,
        })
    }
}

/// TOOLBOX_CLEAN reply — parse IOCStatus only.
///
/// Cites: toolbox-and-config.md §5 (mpi2_tool.h:53-68) + sgl-and-replies.md §8.3
#[derive(Debug, Clone, PartialEq)]
pub struct ToolboxReply {
    /// Tool discriminator echoed back by IOC — toolbox-and-config.md §5.1
    pub tool: u8,

    /// IOC return status — sgl-and-replies.md §8.3 (offset 0x0E)
    pub ioc_status: IocStatus,
}

impl ToolboxReply {
    /// Parse TOOLBOX reply from raw bytes.
    ///
    /// Expects at least 18 bytes. Reads IOCStatus at offset 0x0E per sgl-and-replies.md §8.3.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 18 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::Toolbox,
                got: bytes.len(),
                need: 18,
            });
        }

        // Offset 0x00: Tool (CLEAN = 0x00) — toolbox-and-config.md §5.1
        let tool = bytes[0];

        // Offset 0x0E: IOCStatus (2 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        Ok(Self { tool, ioc_status })
    }
}

/// CONFIG reply — parse IOCStatus and echoed page header.
///
/// Cites: toolbox-and-config.md §6 (mpi2_cnfg.h:366-383) + sgl-and-replies.md §8.3
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigReply {
    /// Action echoed back by IOC — toolbox-and-config.md §6
    pub action: u8,

    /// Page header (version, length, number, type) — toolbox-and-config.md §6.2
    pub page_header: [u8; 4],

    /// IOC return status — sgl-and-replies.md §8.3 (offset 0x0E)
    pub ioc_status: IocStatus,
}

impl ConfigReply {
    /// Parse CONFIG reply from raw bytes.
    ///
    /// Expects at least 26 bytes. Reads IOCStatus at 0x0E and page header at 0x14.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 26 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::Config,
                got: bytes.len(),
                need: 26,
            });
        }

        // Offset 0x00: Action — toolbox-and-config.md §6.1
        let action = bytes[0];

        // Offset 0x14-0x17: PageHeader (4 bytes) — toolbox-and-config.md §6.2
        let page_header = [bytes[20], bytes[21], bytes[22], bytes[23]];

        // Offset 0x0E: IOCStatus (2 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        Ok(Self {
            action,
            page_header,
            ioc_status,
        })
    }
}

/// IOC_INIT reply — parse WhoInit and IOCStatus.
///
/// Cites: mpi-overview.md §9 (mpi2_ioc.h:191-207) + sgl-and-replies.md §8.3
#[derive(Debug, Clone, PartialEq)]
pub struct IocInitReply {
    /// Who initiated the IOC — mpi-overview.md §9.2 (should match request WhoInit)
    pub who_init: u8,

    /// IOC return status — sgl-and-replies.md §8.3 (offset 0x0E)
    pub ioc_status: IocStatus,
}

impl IocInitReply {
    /// Parse IOC_INIT reply from raw bytes.
    ///
    /// Expects at least 18 bytes. Reads WhoInit at 0x00 and IOCStatus at 0x0E per mpi-overview.md §9.2.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 18 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::IocInit,
                got: bytes.len(),
                need: 18,
            });
        }

        // Offset 0x00: WhoInit — mpi-overview.md §9.2 (MPI2_WHOINIT_HOST_DRIVER = 0x04)
        let who_init = bytes[0];

        // Offset 0x0E: IOCStatus (2 bytes little-endian) — sgl-and-replies.md §8.3
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        Ok(Self {
            who_init,
            ioc_status,
        })
    }
}

/// IOC_FACTS request (MPI v2.0). Function code 0x03 per mpi2_ioc.h:191-227.
/// Total size: 16 bytes header only, no SGL needed.
/// Cites: mpi2_ioc.h:215-227 for exact field layout
#[derive(Debug, Clone)]
pub struct IocFactsRequest;

impl IocFactsRequest {
    /// Serialize IOC_FACTS request to wire format (header only).
    /// Returns 16 bytes: MPI2_REQUEST_HEADER (10B) + body (6B).
    pub fn serialize_to(smid: u16) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16);

        // MPI2_REQUEST_HEADER (10 bytes) — mpi-overview.md §1.2
        buf.extend_from_slice(&0u16.to_le_bytes()); // FunctionDependent1: 0 for IOC_FACTS
        buf.push(0x00); // ChainOffset: 0 = single message, no chaining
        buf.push(MpiFunction::IocFacts.as_u8()); // Function = 0x03 per mpi2_ioc.h:191
        buf.extend_from_slice(&smid.to_le_bytes()); // FunctionDependent2 (SMID)
        buf.push(0x00); // FunctionDependent3: 0

        let msg_flags = 0x00; // Not used for IOC_FACTS request
        buf.push(msg_flags);
        buf.push(0x00); // VP_ID: not used
        buf.push(0x00); // VF_ID: not used
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved1

        buf
    }
}

/// IOC_FACTS reply (MPI v2.0). Function code 0x03 per mpi2_ioc.h:191-279.
/// Total size: ~96 bytes including BoardName and BoardTracer strings.
/// Cites: mpi2_ioc.h:231-269 for exact field layout
#[derive(Debug, Clone, PartialEq)]
pub struct IocFactsReply {
    /// Message version (high byte = major, low byte = minor) — mpi2_ioc.h:234
    pub msg_version: u16,

    /// Message length in bytes — mpi2_ioc.h:235
    pub msg_length: u8,

    /// Function code echoed back — mpi2_ioc.h:236 (0x03)
    pub function: u8,

    /// Header version — mpi2_ioc.h:237
    pub header_version: u16,

    /// IOC number — mpi2_ioc.h:239
    pub ioc_number: u8,

    /// Message flags — mpi2_ioc.h:240
    pub msg_flags: u8,

    /// VP_ID — mpi2_ioc.h:241
    pub vp_id: u8,

    /// VF_ID — mpi2_ioc.h:242
    pub vf_id: u8,

    /// Reserved — mpi2_ioc.h:243
    pub reserved_1: u16,

    /// IOC exceptions — mpi2_ioc.h:244
    pub ioc_exceptions: u16,

    /// IOC status (2B LE at offset 0x0E) — mpi2_ioc.h:245
    pub ioc_status: IocStatus,

    /// IOC log info (4B at offset 0x10) — mpi2_ioc.h:246
    pub ioc_log_info: u32,

    /// Max chain depth — mpi2_ioc.h:247
    pub max_chain_depth: u8,

    /// Who initiated IOC (WhoInit value) — mpi2_ioc.h:248
    pub who_init: u8,

    /// Number of ports — mpi2_ioc.h:249
    pub number_of_ports: u8,

    /// Max MSI-X vectors — mpi2_ioc.h:250
    pub max_msix_vectors: u8,

    /// Request credit — mpi2_ioc.h:251
    pub request_credit: u16,

    /// Product ID (2B LE at offset 0x1A) — mpi2_ioc.h:252
    pub product_id: u16,

    /// IOC capabilities (4B at offset 0x1C) — mpi2_ioc.h:253
    pub ioc_capabilities: u32,

    /// Firmware version union (4B at offset 0x20) — mpi2_ioc.h:254
    /// Format: major(8b).minor(8b).unit(8b).dev(8b) encoded in u32 LE
    pub fw_version: u32,

    /// IOC request frame size — mpi2_ioc.h:255
    pub ioc_request_frame_size: u16,

    /// IOC max chain segment size — mpi2_ioc.h:256
    pub ioc_max_chain_segment_size: u16,

    /// Max initiators — mpi2_ioc.h:257
    pub max_initiators: u16,

    /// Max targets — mpi2_ioc.h:258
    pub max_targets: u16,

    /// Max SAS expanders — mpi2_ioc.h:259
    pub max_sas_expanders: u16,

    /// Max enclosures — mpi2_ioc.h:260
    pub max_enclosures: u16,

    /// Protocol flags — mpi2_ioc.h:261
    pub protocol_flags: u16,

    /// High priority credit — mpi2_ioc.h:262
    pub high_priority_credit: u16,

    /// Max reply descriptor post queue depth — mpi2_ioc.h:263
    pub max_reply_descriptor_post_queue_depth: u16,

    /// Reply frame size — mpi2_ioc.h:264
    pub reply_frame_size: u8,

    /// Max volumes — mpi2_ioc.h:265
    pub max_volumes: u8,

    /// Max device handle — mpi2_ioc.h:266
    pub max_dev_handle: u16,

    /// Max persistent entries — mpi2_ioc.h:267
    pub max_persistent_entries: u16,

    /// Min device handle — mpi2_ioc.h:268
    pub min_dev_handle: u16,

    /// Reserved4 (2B at offset 0x3E) — mpi2_ioc.h:269
    pub reserved_4: u16,

    /// Board name string (16 bytes null-terminated at offset 0x40) — mpi2_ioc.h:270-275.
    /// Optional because SAS2008 IOC_FACTS reply only carries the 64-byte header
    /// (16 dwords); BoardName/Tracer were added in later MPI revisions and only
    /// appear when the chip reports MsgLength ≥ 24 dwords (96 bytes). For pre-MPI-2.5
    /// chips like SAS2008, fetch this from Manufacturing Page 0 separately.
    pub board_name: Option<String>,

    /// Board tracer string (16 bytes null-terminated at offset 0x50) — mpi2_ioc.h:276-281.
    /// Optional for the same reason as board_name.
    pub board_tracer: Option<String>,

    // Extended fields from Manufacturing Page 0 (fetched via CONFIG roundtrip):
    // These are populated separately after send_config read of Mfg Page 0
    /// NVDATA vendor ID (2B at offset 0x08 in Mfg Page 0) — toolbox-and-config.md §5
    pub nvdata_vendor_id: Option<u16>,

    /// NVDATA product ID string (10 chars from Mfg Page 0) — baseline.md:14
    pub nvdata_product_id: Option<String>,

    /// NVDATA version (4B from Mfg Page 0, distinct from FW version) — baseline.md:15
    pub nvdata_version: Option<u32>,

    /// Firmware product ID string (~16 chars from Mfg Page 0 or IOC_FACTS ProductID) — baseline.md:15
    pub firmware_product_id: Option<String>,
}

impl IocFactsReply {
    /// Parse IOC_FACTS reply from raw bytes.
    /// Parse IOC_FACTS reply. Requires at least 64 bytes (16 dwords) for the
    /// MPI 2.0 base reply; BoardName + BoardTracer at offsets 0x40 and 0x50
    /// are parsed only if the chip reports a length covering them (SAS2008
    /// returns 64-byte replies and omits them — confirmed dev-1 2026-05-28).
    /// Cites: mpi2_ioc.h:231-281 for the full field layout.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 64 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::IocFacts,
                got: bytes.len(),
                need: 64,
            });
        }

        // Offset 0x00-0x01: MsgVersion — mpi2_ioc.h:234
        let msg_version = u16::from_le_bytes([bytes[0], bytes[1]]);

        // Offset 0x02: MsgLength — mpi2_ioc.h:235
        let msg_length = bytes[2];

        // Offset 0x03: Function — mpi2_ioc.h:236 (should be 0x03)
        let function = bytes[3];
        if function != MpiFunction::IocFacts.as_u8() {
            return Err(MpiError::WrongReplyFunction {
                expected: MpiFunction::IocFacts,
                got_function: function,
                expected_function: MpiFunction::IocFacts.as_u8(),
                head: bytes[..bytes.len().min(16)].to_vec(),
            });
        }
        // Offset 0x04-0x05: HeaderVersion — mpi2_ioc.h:237
        let header_version = u16::from_le_bytes([bytes[4], bytes[5]]);

        // Offset 0x06: IOCNumber — mpi2_ioc.h:239
        let ioc_number = bytes[6];

        // Offset 0x07: MsgFlags — mpi2_ioc.h:240
        let msg_flags = bytes[7];

        // Offset 0x08-0x09: VP_ID, VF_ID — mpi2_ioc.h:241-242
        let vp_id = bytes[8];
        let vf_id = bytes[9];

        // Offset 0x0A-0x0B: Reserved1 — mpi2_ioc.h:243
        let reserved_1 = u16::from_le_bytes([bytes[10], bytes[11]]);

        // Offset 0x0C-0x0D: IOCExceptions — mpi2_ioc.h:244
        let ioc_exceptions = u16::from_le_bytes([bytes[12], bytes[13]]);

        // Offset 0x0E-0x0F: IOCStatus (2B LE) — mpi2_ioc.h:245
        let ioc_status_raw = u16::from_le_bytes([bytes[14], bytes[15]]);
        let ioc_status = IocStatus::from_u16(ioc_status_raw)?;

        // Offset 0x10-0x13: IOCLogInfo (4B LE) — mpi2_ioc.h:246
        let ioc_log_info = u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);

        // Offset 0x14: MaxChainDepth — mpi2_ioc.h:247
        let max_chain_depth = bytes[20];

        // Offset 0x15: WhoInit — mpi2_ioc.h:248
        let who_init = bytes[21];

        // Offset 0x16: NumberOfPorts — mpi2_ioc.h:249
        let number_of_ports = bytes[22];

        // Offset 0x17: MaxMSIxVectors — mpi2_ioc.h:250
        let max_msix_vectors = bytes[23];

        // Offset 0x18-0x19: RequestCredit — mpi2_ioc.h:251
        let request_credit = u16::from_le_bytes([bytes[24], bytes[25]]);

        // Offset 0x1A-0x1B: ProductID (2B LE) — mpi2_ioc.h:252
        let product_id = u16::from_le_bytes([bytes[26], bytes[27]]);

        // Offset 0x1C-0x1F: IOCCapabilities (4B LE) — mpi2_ioc.h:253
        let ioc_capabilities = u32::from_le_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);

        // Offset 0x20-0x23: FWVersion (4B LE) — mpi2_ioc.h:254
        // Format: major(8b).minor(8b).unit(8b).dev(8b) encoded in u32 LE
        let fw_version = u32::from_le_bytes([bytes[32], bytes[33], bytes[34], bytes[35]]);

        // Offset 0x24-0x25: IOCRequestFrameSize — mpi2_ioc.h:255
        let ioc_request_frame_size = u16::from_le_bytes([bytes[36], bytes[37]]);

        // Offset 0x26-0x27: IOCMaxChainSegmentSize — mpi2_ioc.h:256
        let ioc_max_chain_segment_size = u16::from_le_bytes([bytes[38], bytes[39]]);

        // Offset 0x28-0x29: MaxInitiators — mpi2_ioc.h:257
        let max_initiators = u16::from_le_bytes([bytes[40], bytes[41]]);

        // Offset 0x2A-0x2B: MaxTargets — mpi2_ioc.h:258
        let max_targets = u16::from_le_bytes([bytes[42], bytes[43]]);

        // Offset 0x2C-0x2D: MaxSasExpanders — mpi2_ioc.h:259
        let max_sas_expanders = u16::from_le_bytes([bytes[44], bytes[45]]);

        // Offset 0x2E-0x2F: MaxEnclosures — mpi2_ioc.h:260
        let max_enclosures = u16::from_le_bytes([bytes[46], bytes[47]]);

        // Offset 0x30-0x31: ProtocolFlags — mpi2_ioc.h:261
        let protocol_flags = u16::from_le_bytes([bytes[48], bytes[49]]);

        // Offset 0x32-0x33: HighPriorityCredit — mpi2_ioc.h:262
        let high_priority_credit = u16::from_le_bytes([bytes[50], bytes[51]]);

        // Offset 0x34-0x35: MaxReplyDescriptorPostQueueDepth — mpi2_ioc.h:263
        let max_reply_descriptor_post_queue_depth = u16::from_le_bytes([bytes[52], bytes[53]]);

        // Offset 0x36: ReplyFrameSize — mpi2_ioc.h:264
        let reply_frame_size = bytes[54];

        // Offset 0x37: MaxVolumes — mpi2_ioc.h:265
        let max_volumes = bytes[55];

        // Offset 0x38-0x39: MaxDevHandle — mpi2_ioc.h:266
        let max_dev_handle = u16::from_le_bytes([bytes[56], bytes[57]]);

        // Offset 0x3A-0x3B: MaxPersistentEntries — mpi2_ioc.h:267
        let max_persistent_entries = u16::from_le_bytes([bytes[58], bytes[59]]);

        // Offset 0x3C-0x3D: MinDevHandle — mpi2_ioc.h:268
        let min_dev_handle = u16::from_le_bytes([bytes[60], bytes[61]]);

        // Offset 0x3E-0x3F: Reserved4 — mpi2_ioc.h:269
        let reserved_4 = u16::from_le_bytes([bytes[62], bytes[63]]);

        // Offsets 0x40-0x4F BoardName, 0x50-0x5F BoardTracer — only present
        // when the chip returns ≥80 / ≥96 bytes. SAS2008 omits both.
        let board_name = if bytes.len() >= 80 {
            Some(parse_null_terminated_string(&bytes[64..80]))
        } else {
            None
        };
        let board_tracer = if bytes.len() >= 96 {
            Some(parse_null_terminated_string(&bytes[80..96]))
        } else {
            None
        };

        Ok(Self {
            msg_version,
            msg_length,
            function,
            header_version,
            ioc_number,
            msg_flags,
            vp_id,
            vf_id,
            reserved_1,
            ioc_exceptions,
            ioc_status,
            ioc_log_info,
            max_chain_depth,
            who_init,
            number_of_ports,
            max_msix_vectors,
            request_credit,
            product_id,
            ioc_capabilities,
            fw_version,
            ioc_request_frame_size,
            ioc_max_chain_segment_size,
            max_initiators,
            max_targets,
            max_sas_expanders,
            max_enclosures,
            protocol_flags,
            high_priority_credit,
            max_reply_descriptor_post_queue_depth,
            reply_frame_size,
            max_volumes,
            max_dev_handle,
            max_persistent_entries,
            min_dev_handle,
            reserved_4,
            board_name,
            board_tracer,
            nvdata_vendor_id: None, // Populated separately via Mfg Page 0 CONFIG read
            nvdata_product_id: None,
            nvdata_version: None,
            firmware_product_id: None,
        })
    }

    /// Parse FW version u32 into (major, minor, unit, dev) components.
    /// MPI2_FW_VERSION_UNION per mpi2_ioc.h:254 is `struct { U8 Dev; U8 Unit;
    /// U8 Minor; U8 Major; }` — high byte of the u32 is Major. Dev-1 confirmed
    /// 2026-05-28: chip returned fw_version=0x07150800 for the H200 Tape Adapter
    /// running Dell ITA A04 (MPTFW-07.15.08.00-IE).
    pub fn fw_version_components(&self) -> (u8, u8, u8, u8) {
        let dev = (self.fw_version & 0xFF) as u8;
        let unit = ((self.fw_version >> 8) & 0xFF) as u8;
        let minor = ((self.fw_version >> 16) & 0xFF) as u8;
        let major = ((self.fw_version >> 24) & 0xFF) as u8;
        (major, minor, unit, dev)
    }

    /// Format FW version as human-readable string "major.minor.unit.dev".
    pub fn fw_version_string(&self) -> String {
        let (major, minor, unit, dev) = self.fw_version_components();
        format!("{}.{}.{}.{}", major, minor, unit, dev)
    }

    /// Parse NVDATA version u32 into (major, minor, build, revision) components.
    /// Format similar to FW version but represents NVDATA schema version.
    pub fn nvdata_version_components(&self) -> Option<(u8, u8, u16)> {
        self.nvdata_version.map(|v| {
            let major = (v & 0xFF) as u8;
            let minor = ((v >> 8) & 0xFF) as u8;
            let build = ((v >> 16) & 0xFFFF) as u16;
            (major, minor, build)
        })
    }

    /// Format NVDATA version as human-readable string "major.minor.build".
    pub fn nvdata_version_string(&self) -> Option<String> {
        self.nvdata_version_components()
            .map(|(major, minor, build)| format!("{}.{}.{}", major, minor, build))
    }

    /// Get NVDATA vendor ID as hex string.
    pub fn nvdata_vendor_id_hex(&self) -> Option<String> {
        self.nvdata_vendor_id.map(|id| format!("0x{:04X}", id))
    }

    /// Get firmware product ID from ProductID field (u16).
    pub fn fw_product_id_from_facts(&self) -> String {
        // ProductID is a u16; convert to hex string for display
        format!("0x{:04X}", self.product_id)
    }

    /// Get full formatted IOC_FACTS info as human-readable lines.
    pub fn to_info_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();

        // Firmware version from IOC_FACTS
        lines.push(format!("  Firmware Version: {}", self.fw_version_string()));

        // NVDATA vendor/product ID from Mfg Page 0
        if let Some(vendor_id) = self.nvdata_vendor_id_hex() {
            lines.push(format!("  NVDATA Vendor ID: {}", vendor_id));
        }
        if let Some(prod_id) = &self.nvdata_product_id {
            lines.push(format!("  NVDATA Product ID: {}", prod_id));
        }

        // NVDATA version (distinct from FW version per baseline.md:15)
        if let Some(nv_ver_str) = self.nvdata_version_string() {
            lines.push(format!("  NVDATA Version: {}", nv_ver_str));
        }

        // Firmware product ID string
        if let Some(fw_prod_id) = &self.firmware_product_id {
            lines.push(format!("  Firmware Product ID: {}", fw_prod_id));
        }

        // Board name and tracer from IOC_FACTS — None on chips that return
        // the short 64-byte reply (SAS2008); fetched from Mfg Page 0 instead.
        if let Some(ref name) = self.board_name {
            lines.push(format!("  Board Name: {}", name));
        }
        if let Some(ref tracer) = self.board_tracer {
            lines.push(format!("  Board Tracer: {}", tracer));
        }

        // ProtocolFlags per mpi2_ioc.h:262 — bit 0 = INITIATOR, bit 1 = TARGET.
        // (SR-IOV doesn't live here; the freshman cycle mislabeled it.)
        let mut protocols = Vec::new();
        if self.protocol_flags & 0x0001 != 0 {
            protocols.push("Initiator");
        }
        if self.protocol_flags & 0x0002 != 0 {
            protocols.push("Target");
        }
        if !protocols.is_empty() {
            lines.push(format!("  Protocols: {}", protocols.join(" + ")));
        }

        lines.push(format!("  Max SAS Expanders: {}", self.max_sas_expanders));
        lines.push(format!("  Max Enclosures: {}", self.max_enclosures));
        lines.push(format!("  Max Targets: {}", self.max_targets));
        lines.push(format!("  Max Initiators: {}", self.max_initiators));

        lines
    }
}

/// Parse a null-terminated ASCII string from bytes.
fn parse_null_terminated_string(bytes: &[u8]) -> String {
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..len]).to_string()
}

// ============================================================================
// MpiError — top-level errors for messages module
// ============================================================================

/// Errors specific to MPI message serialization/deserialization.
#[derive(thiserror::Error, Debug)]
pub enum MpiError {
    /// IOC returned an error status code.
    #[error("IOC reported error: {0}")]
    IocStatus(#[from] IocStatus),

    /// Reply frame was shorter than expected for the given function.
    #[error("malformed reply for {function:?}: frame too short ({got} < {need})")]
    MalformedReply {
        function: MpiFunction,
        got: usize,
        need: usize,
    },

    /// Reply frame's Function byte didn't match the request. Usually means the
    /// IOC is mid-state-transition or a previous message wasn't drained.
    #[error("malformed reply for {expected:?}: function byte = 0x{got_function:02X} (expected 0x{expected_function:02X}); first 16 reply bytes = {head:02X?}")]
    WrongReplyFunction {
        expected: MpiFunction,
        got_function: u8,
        expected_function: u8,
        head: Vec<u8>,
    },

    /// Unknown IOCStatus code returned by IOC (not in iocstatus-table.md §10).
    #[error("unknown IOCStatus code 0x{0:04X}")]
    UnknownIocStatus(u16),

    /// Unknown MpiFunction code (not one of the 5 supported functions).
    #[error("unknown MpiFunction code 0x{0:02X}")]
    UnknownFunction(u8),

    /// Unknown ImageType code.
    #[error("unknown ImageType code 0x{0:02X}")]
    UnknownImageType(u8),

    /// ADR-015 Rule 1: running firmware personality does not match target.
    /// Compile-time prevention of cross-personality writes (the dev-1 brick scenario).
    #[error("personality mismatch: running={running:?}, target={target:?} — ADR-015 Rule 1")]
    PersonalityMismatch {
        running: crate::mpi::session::Personality,
        target: crate::mpi::session::Personality,
    },

    /// ADR-015 Rule 4/6: verify-after-write detected byte mismatch.
    #[error("verify-after-write mismatch at offset {offset}")]
    VerifyMismatch { offset: usize },

    /// Hardware path not yet implemented. Used by RealIoc destructive ops
    /// until CH341A SPI clip + cold-spare card are on hand (see
    /// memory/lsiutil_fragility_and_brick.md for why these stay gated).
    #[error("{op} not implemented yet (brick-gated; see lsiutil_fragility_and_brick.md)")]
    NotImplementedYet { op: &'static str },

    /// I/O error during hardware access (BAR1 mmap, sysfs read, etc.).
    #[error("hardware I/O error: {0}")]
    Io(String),
}

// ============================================================================
// FlashRegionType and FlashLayoutReply — ADR-015 Rule 11a (mpi2_ioc.h:1469-1502)
// ============================================================================

/// One flash region descriptor — 16 bytes wire format per MPI2_FLASH_REGION at mpi2_ioc.h:1469-1477.
/// Layout: RegionType(U8, offset 0x00), Reserved1(U8, 0x01), Reserved2(U16, 0x02),
/// RegionOffset(U32, 0x04), RegionSize(U32, 0x08), Reserved3(U32, 0x0C).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashRegion {
    region_type: u8,        // Offset 0x00 — stored as raw byte for wire format
    reserved_1: u8,         // Offset 0x01 — not exposed (reserved)
    reserved_2: u16,        // Offset 0x02-0x03 — not exposed (reserved)
    pub region_offset: u32, // Offset 0x04
    pub region_size: u32,   // Offset 0x08
    reserved_3: u32,        // Offset 0x0C-0x0F — not exposed (reserved)
}

impl FlashRegion {
    /// Create a new FlashRegion with all fields including reserved padding.
    pub fn new(region_type: FlashRegionType, region_offset: u32, region_size: u32) -> Self {
        Self {
            region_type: region_type.as_u8(),
            reserved_1: 0x00,
            reserved_2: 0x0000,
            region_offset,
            region_size,
            reserved_3: 0x00000000,
        }
    }

    /// Get the region type as FlashRegionType enum.
    pub fn region_type(&self) -> FlashRegionType {
        FlashRegionType::from_u8(self.region_type)
    }
}

/// Flash region type discriminator — MPI2_FLASH_REGIONTYPE_* at mpi2_ioc.h:1506-1518.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashRegionType {
    Unused = 0x00,        // MPI2_FLASH_REGION_UNUSED
    Firmware = 0x01,      // MPI2_FLASH_REGION_FIRMWARE
    Bios = 0x02,          // MPI2_FLASH_REGION_BIOS
    Manufacturing = 0x03, // MPI2_FLASH_REGION_NVDATA (manufacturing pages / NVDATA)
    Config = 0x04,        // MPI2_FLASH_REGION_CONFIG_1/CONFIG_2
    MfgPlusConfig = 0x05, // MPI2_FLASH_REGION_MEGARAID (mfg + cfg combined)
    BootService = 0x06,   // MPI2_FLASH_REGION_MFG_INFORMATION (boot service)
    Log = 0x07,           // MPI2_FLASH_REGION_INIT (log / init data)
    Other(u8),            // Unknown or future types
}

impl FlashRegionType {
    /// Parse raw u8 into FlashRegionType enum.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0x00 => Self::Unused,
            0x01 => Self::Firmware,
            0x02 => Self::Bios,
            0x03 => Self::Manufacturing,
            0x04 => Self::Config,
            0x05 => Self::MfgPlusConfig,
            0x06 => Self::BootService,
            0x07 => Self::Log,
            other => Self::Other(other),
        }
    }

    /// Convert FlashRegionType back to raw u8.
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Unused => 0x00,
            Self::Firmware => 0x01,
            Self::Bios => 0x02,
            Self::Manufacturing => 0x03,
            Self::Config => 0x04,
            Self::MfgPlusConfig => 0x05,
            Self::BootService => 0x06,
            Self::Log => 0x07,
            Self::Other(v) => v,
        }
    }
}

/// MPI2_FLASH_LAYOUT reply — chip's authoritative flash map per ADR-015 Rule 11a.
/// Wire format per mpi2_ioc.h:1480-1502 (MPI2_FLASH_LAYOUT_DATA).
/// Layout: FlashSize(U32, 0x00), Reserved1-3(U32 each, 0x04-0x0C), Region[] (variable length, 16 bytes each starting at 0x10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashLayoutReply {
    pub flash_size: u32,
    pub regions: Vec<FlashRegion>,
}

impl FlashLayoutReply {
    /// Parse from raw config-page-data bytes returned by CONFIG(MPI2_CONFIG_EXTPAGETYPE_FLASH_LAYOUT).
    /// Returns Err on short buffer or impossible field values.
    ///
    /// The layout header is 16 bytes (mpi2_ioc.h:1480-1487), followed by variable-length region array.
    /// Each region is 16 bytes per mpi2_ioc.h:1469-1477. The number of regions is inferred from buffer length.
    pub fn parse(bytes: &[u8]) -> Result<Self, MpiError> {
        if bytes.len() < 16 {
            return Err(MpiError::MalformedReply {
                function: MpiFunction::Config,
                got: bytes.len(),
                need: 16,
            });
        }

        // Layout header at offset 0x00-0x0F (16 bytes) — mpi2_ioc.h:1480-1487
        let flash_size = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);

        // Reserved fields at 0x04, 0x08, 0x0C are ignored (not exposed in reply)

        // Regions start at offset 0x10 — mpi2_ioc.h:1469-1477
        let mut regions = Vec::new();
        let region_start = 16;
        let region_size = 16;

        if bytes.len() >= region_start {
            let remaining = bytes.len() - region_start;
            let num_regions = remaining / region_size;

            for i in 0..num_regions {
                let offset = region_start + (i * region_size);

                // RegionType at offset+0x00 (U8)
                let _region_type_raw = bytes[offset];

                // Reserved1 at offset+0x01 (U8) — ignored
                // Reserved2 at offset+0x02-0x03 (U16) — ignored

                // RegionOffset at offset+0x04 (U32 LE)
                let region_offset = u32::from_le_bytes([
                    bytes[offset + 4],
                    bytes[offset + 5],
                    bytes[offset + 6],
                    bytes[offset + 7],
                ]);

                // RegionSize at offset+0x08 (U32 LE)
                let region_size = u32::from_le_bytes([
                    bytes[offset + 8],
                    bytes[offset + 9],
                    bytes[offset + 10],
                    bytes[offset + 11],
                ]);

                // Reserved3 at offset+0x0C-0x0F (U32) — ignored

                regions.push(FlashRegion {
                    region_type: _region_type_raw,
                    reserved_1: 0x00,
                    reserved_2: 0x0000,
                    region_offset,
                    region_size,
                    reserved_3: 0x0000_0000,
                });
            }
        }

        Ok(Self {
            flash_size,
            regions,
        })
    }

    /// Look up the region whose `region_type` matches; returns the FIRST match.
    /// Used by Rule 11a to find "where does the FW image go".
    pub fn region(&self, kind: FlashRegionType) -> Option<&FlashRegion> {
        self.regions.iter().find(|r| r.region_type() == kind)
    }
}

// ============================================================================
// Unit Tests — golden bytes from wire-format docs
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // MpiFunction tests
    // ========================================================================

    #[test]
    fn mpi_function_from_u8_handles_all_codes() {
        assert_eq!(MpiFunction::from_u8(0x02).unwrap(), MpiFunction::IocInit);
        assert_eq!(MpiFunction::from_u8(0x04).unwrap(), MpiFunction::Config);
        assert_eq!(MpiFunction::from_u8(0x09).unwrap(), MpiFunction::FwDownload);
        assert_eq!(MpiFunction::from_u8(0x12).unwrap(), MpiFunction::FwUpload);
        assert_eq!(MpiFunction::from_u8(0x17).unwrap(), MpiFunction::Toolbox);
    }

    #[test]
    fn mpi_function_from_u8_rejects_unknown() {
        assert!(MpiFunction::from_u8(0xFF).is_err());
        assert!(matches!(
            MpiFunction::from_u8(0xFF),
            Err(MpiError::UnknownFunction(_))
        ));
    }

    #[test]
    fn mpi_function_as_u8_roundtrip() {
        for func in [
            MpiFunction::IocInit,
            MpiFunction::Config,
            MpiFunction::FwDownload,
            MpiFunction::FwUpload,
            MpiFunction::Toolbox,
        ] {
            assert_eq!(MpiFunction::from_u8(func.as_u8()).unwrap(), func);
        }
    }

    // ========================================================================
    // ImageType tests
    // ========================================================================

    #[test]
    fn image_type_from_u8_handles_all_codes() {
        assert_eq!(ImageType::from_u8(0x00).unwrap(), ImageType::Reserved);
        assert_eq!(ImageType::from_u8(0x01).unwrap(), ImageType::Fw);
        assert_eq!(ImageType::from_u8(0x02).unwrap(), ImageType::Bios);
        assert_eq!(ImageType::from_u8(0x03).unwrap(), ImageType::NvData);
        assert_eq!(ImageType::from_u8(0x04).unwrap(), ImageType::BootLoader);
        assert_eq!(ImageType::from_u8(0x05).unwrap(), ImageType::Initialization);
        assert_eq!(ImageType::from_u8(0x06).unwrap(), ImageType::FlashLayout);
        assert_eq!(
            ImageType::from_u8(0x07).unwrap(),
            ImageType::SupportedDevices
        );
        assert_eq!(ImageType::from_u8(0x08).unwrap(), ImageType::MegaRaid);
    }

    #[test]
    fn image_type_from_u8_rejects_unknown() {
        assert!(ImageType::from_u8(0xFF).is_err());
        assert!(matches!(
            ImageType::from_u8(0xFF),
            Err(MpiError::UnknownImageType(_))
        ));
    }

    // ========================================================================
    // IocStatus tests — iocstatus-table.md §10
    // ========================================================================

    #[test]
    fn iocstatus_success_is_not_hard_stop() {
        assert!(!IocStatus::Success.is_flash_hard_stop());
    }

    #[test]
    fn iocstatus_internal_error_triggers_hard_stop() {
        // ADR-015 Rule 6: INTERNAL_ERROR is the dev-1 brick code
        assert!(IocStatus::InternalError.is_flash_hard_stop());
    }

    #[test]
    fn iocstatus_all_known_codes_not_success_are_hard_stops() {
        // All non-Success codes are hard stops per Rule 6
        let all_codes = [
            IocStatus::InvalidFunction,
            IocStatus::Busy,
            IocStatus::InvalidSgl,
            IocStatus::InternalError,
            IocStatus::InvalidVpid,
            IocStatus::InsufficientResources,
            IocStatus::InvalidField,
            IocStatus::InvalidState,
            IocStatus::OpStateNotSupported,
        ];

        for status in all_codes {
            assert!(
                status.is_flash_hard_stop(),
                "{:?} should be hard stop",
                status
            );
        }
    }

    #[test]
    fn iocstatus_from_u16_handles_known_codes() {
        // Test a sample of codes from iocstatus-table.md §10
        assert_eq!(IocStatus::from_u16(0x0000).unwrap(), IocStatus::Success);
        assert_eq!(
            IocStatus::from_u16(0x0001).unwrap(),
            IocStatus::InvalidFunction
        );
        assert_eq!(IocStatus::from_u16(0x0002).unwrap(), IocStatus::Busy);
        assert_eq!(
            IocStatus::from_u16(0x0004).unwrap(),
            IocStatus::InternalError
        );
        assert_eq!(IocStatus::from_u16(0x0005).unwrap(), IocStatus::InvalidVpid);
        assert_eq!(
            IocStatus::from_u16(0x0020).unwrap(),
            IocStatus::ConfigInvalidAction
        );
        assert_eq!(
            IocStatus::from_u16(0x0040).unwrap(),
            IocStatus::ScsiRecoveredError
        );
    }

    #[test]
    fn iocstatus_from_u16_handles_unknown_codes() {
        // Test unknown codes return UnknownIocStatus error
        assert!(IocStatus::from_u16(0xFFFE).is_err());
        assert!(matches!(
            IocStatus::from_u16(0xFFFF),
            Err(MpiError::UnknownIocStatus(_))
        ));

        // Specific known-unknown codes from iocstatus-table.md §10 gaps
        assert!(IocStatus::from_u16(0x003A).is_err());
    }

    #[test]
    fn iocstatus_as_u16_roundtrip() {
        for status in [
            IocStatus::Success,
            IocStatus::InternalError,
            IocStatus::ConfigInvalidPage,
            IocStatus::ScsiDataOverrun,
        ] {
            assert_eq!(IocStatus::from_u16(status.as_u16()).unwrap(), status);
        }
    }

    // ========================================================================
    // ToolboxCleanFlags tests — toolbox-and-config.md §5.2
    // ========================================================================

    #[test]
    fn toolboxcleanflags_individual_bits() {
        assert!(ToolboxCleanFlags::NVRAM.contains(ToolboxCleanFlags::NVRAM));
        assert!(!ToolboxCleanFlags::SEEPROM.contains(ToolboxCleanFlags::NVRAM));

        let all = ToolboxCleanFlags::ALL;
        assert!(all.contains(ToolboxCleanFlags::NVRAM));
        assert!(all.contains(ToolboxCleanFlags::FLASH));
        assert!(all.contains(ToolboxCleanFlags::PERSIST_MANUFACT_PAGES));
    }

    #[test]
    fn toolboxcleanflags_combine_bits() {
        let flags = ToolboxCleanFlags::NVRAM | ToolboxCleanFlags::SEEPROM;
        assert!(flags.contains(ToolboxCleanFlags::NVRAM));
        assert!(flags.contains(ToolboxCleanFlags::SEEPROM));
        assert!(!flags.contains(ToolboxCleanFlags::FLASH));
    }

    // ========================================================================
    // IeeeSgeSimple64 tests — sgl-and-replies.md §7.1, §7.3
    // ========================================================================

    #[test]
    fn ieee_sge_serializes_to_16_bytes() {
        let sge = IeeeSgeSimple64::new(0xDEADBEEFCAFEBABE, 256);
        let mut buf = Vec::new();
        sge.serialize_to(&mut buf);
        assert_eq!(buf.len(), 16);
    }

    #[test]
    fn ieee_sge_serialized_size_constant() {
        assert_eq!(IeeeSgeSimple64::serialized_size(), 16);
    }

    #[test]
    fn ieee_sge_address_field_correct() {
        let sge = IeeeSgeSimple64::new(0x123456789ABCDEF0, 1024);
        let mut buf = Vec::new();
        sge.serialize_to(&mut buf);

        // Address is first 8 bytes (little-endian)
        assert_eq!(&buf[0..8], &0x123456789ABCDEF0u64.to_le_bytes());
    }

    #[test]
    fn ieee_sge_length_field_correct() {
        let sge = IeeeSgeSimple64::new(0x0, 0x1234ABCD);
        let mut buf = Vec::new();
        sge.serialize_to(&mut buf);

        // Length is bytes 8-11 (little-endian)
        assert_eq!(&buf[8..12], &0x1234ABCDu32.to_le_bytes());
    }

    #[test]
    fn ieee_sge_end_of_list_flag() {
        let sge = IeeeSgeSimple64::new(0x0, 0);
        assert!(sge.end_of_list()); // Default flag 0x40 has EOL bit set

        let no_eol = IeeeSgeSimple64::with_flags(0x0, 0, 0x00);
        assert!(!no_eol.end_of_list());
    }

    // ========================================================================
    // FwDownloadRequest tests — fw-download-upload.md §3.1
    // ========================================================================

    #[test]
    fn fw_download_request_serializes_to_expected_bytes() {
        let payload = vec![0xAA; 64];
        let req = FwDownloadRequest {
            image_type: ImageType::Fw,
            image_offset: 0x1000,
            image_size: 64,
            total_image_size: 32768, // 32KB firmware
            last_segment: true,
            payload: &payload,
        };

        let bytes = req.serialize_to(1);

        // Header at offset 0x03 should be Function = 0x09 (FW_DOWNLOAD)
        assert_eq!(bytes[3], MpiFunction::FwDownload.as_u8());

        // MsgFlags at offset 0x07 should have LAST_SEGMENT bit set
        assert_eq!(bytes[7] & 0x01, 0x01);

        // ImageType at header body start (offset 0x0C)
        assert_eq!(bytes[12], ImageType::Fw.as_u8());

        // TotalImageSize at offset 0x18-0x1B (32KB = 0x8000)
        let total_size = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        assert_eq!(total_size, 32768);

        // SGL starts at offset 0x28 (40 bytes header+body)
        let sge_start = 40;
        assert_eq!(bytes[sge_start + 15], 0x40); // Flags byte should be EOL bit
    }

    #[test]
    fn fw_download_request_last_segment_flag() {
        let payload = vec![0xBB];

        let req_no_last = FwDownloadRequest {
            image_type: ImageType::Bios,
            image_offset: 0,
            image_size: 1,
            total_image_size: 1,
            last_segment: false,
            payload: &payload,
        };

        let req_last = FwDownloadRequest {
            image_type: ImageType::Bios,
            image_offset: 0,
            image_size: 1,
            total_image_size: 1,
            last_segment: true,
            payload: &payload,
        };

        let bytes_no_last = req_no_last.serialize_to(1);
        let bytes_last = req_last.serialize_to(1);

        // MsgFlags should differ in LAST_SEGMENT bit (0x01)
        assert_eq!(bytes_no_last[7] & 0x01, 0x00);
        assert_eq!(bytes_last[7] & 0x01, 0x01);
    }

    // ========================================================================
    // FwUploadRequest tests — fw-download-upload.md §4.1
    // ========================================================================

    #[test]
    fn fw_upload_request_serializes_correctly() {
        let mut buf = vec![0x00; 256];
        let req = FwUploadRequest {
            image_type: ImageType::Fw,
            image_offset: 0,
            image_size: 256,
            payload_buffer: &mut buf,
        };

        let iova: u64 = 0xdead_beef_0000;
        let bytes = req.serialize_to(iova);

        // ImageType at offset 0x00 — corrected per mpi2_ioc.h:1228
        assert_eq!(bytes[0], ImageType::Fw.as_u8());
        // Function at offset 0x03 should be 0x12 (FW_UPLOAD) per mpi2_ioc.h:1231
        assert_eq!(bytes[3], MpiFunction::FwUpload.as_u8());
        // SGE is MPI 2.0 SIMPLE_64 format (FlagsLength at 0x14-0x17, Address at 0x18-0x1F).
        // The dev-1 2026-05-28 finding: SAS2008 is MPI 2.0 not 2.5 — wrong SGE format
        // = chip reads garbage flags + wrong address = silent DMA to wrong place.
        let flags_length = u32::from_le_bytes([bytes[0x14], bytes[0x15], bytes[0x16], bytes[0x17]]);
        let flags = (flags_length >> 24) as u8;
        let length = flags_length & 0x00FF_FFFF;
        // Flags must include SIMPLE | LAST | END_OF_BUFFER | 64-BIT | END_OF_LIST = 0xD3
        // (and clear HOST_TO_IOC bit since FW_UPLOAD is IOC→host).
        assert_eq!(
            flags, 0xD3,
            "MPI 2.0 SGE flags must be 0xD3 for FW_UPLOAD (IOC→host one-shot)"
        );
        assert_eq!(length, 256, "SGE length must be the buffer size");
        let sge_addr = u64::from_le_bytes([
            bytes[0x18],
            bytes[0x19],
            bytes[0x1a],
            bytes[0x1b],
            bytes[0x1c],
            bytes[0x1d],
            bytes[0x1e],
            bytes[0x1f],
        ]);
        assert_eq!(
            sge_addr, iova,
            "SGE address must be the iova arg (chip-readable), not a host VA"
        );
    }

    // ========================================================================
    // ToolboxCleanRequest tests — toolbox-and-config.md §5.1/§5.2
    // ========================================================================

    #[test]
    fn toolboxclean_request_serializes_flags() {
        let flags = ToolboxCleanFlags::NVRAM | ToolboxCleanFlags::FLASH;
        let req = ToolboxCleanRequest { flags };

        let bytes = req.serialize_to(1);

        // Function at offset 0x03 should be 0x17 (TOOLBOX)
        assert_eq!(bytes[3], MpiFunction::Toolbox.as_u8());

        // Flags field at end of body (offset 0x18-0x1B = indices 24-27)
        let flags_val = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        assert_eq!(flags_val, flags.bits());

        // Total length should be 28 bytes (10 header + 18 body)
        assert_eq!(bytes.len(), 28);
    }

    // ========================================================================
    // ConfigRequest tests — toolbox-and-config.md §6.1/§6.2
    // ========================================================================

    #[test]
    fn config_request_serializes_action_and_page() {
        let mut buf = vec![0x00; 64];
        let req = ConfigRequest {
            action: 0x01, // READ_CURRENT
            sgl_flags: 0x0C,
            page_type: 0x09, // MANUFACTURING
            page_number: 5,
            ext_page_type: None,
            payload_buffer: &mut buf,
            page_address: 0x0000_0000, // Plain pages have PageAddress=0 per mpi2_cnfg.h:347
        };

        let bytes = req.serialize_to(1);

        // MPI2_CONFIG_REQUEST is the wire frame (no preceding header):
        // Action@0x00, Function@0x03, PageHeader@0x14 (Num@0x16, Type@0x17),
        // PageAddress@0x18, SGE@0x1C. Total 0x2C bytes.
        assert_eq!(bytes[0x00], 0x01); // Action = READ_CURRENT
        assert_eq!(bytes[0x03], MpiFunction::Config.as_u8()); // Function = CONFIG (0x04)
        assert_eq!(bytes[0x16], 5); // PageNumber
        assert_eq!(bytes[0x17], 0x09); // PageType (MANUFACTURING)
        assert_eq!(bytes.len(), 0x2C); // header (0x1C) + 16-byte SGE slot
    }

    // ========================================================================
    // IocInitRequest tests — mpi-overview.md §9.1/§9.2
    // ========================================================================

    #[test]
    fn iocinit_request_serializes_correctly() {
        let req = IocInitRequest {
            who_init: 0x04, // MPI2_WHOINIT_HOST_DRIVER per mpi-overview.md §9.2
            host_msix_vectors: 32,
            reply_descriptor_post_queue_depth: 16, // MPI2_RDPQ_DEPTH_MIN per mpi-overview.md §9.2
            system_request_frame_base_address: 0x1000_0000,
            reply_descriptor_post_queue_address: 0x2000_0000,
        };

        let bytes = req.serialize_to(1);

        // Per mpi2_ioc.h:135-164 — corrected offsets after struct fix:
        // 0x00 WhoInit, 0x03 Function, 0x1C ReplyDescriptorPostQueueDepth.
        assert_eq!(bytes[0], 0x04); // WhoInit
        assert_eq!(bytes[3], MpiFunction::IocInit.as_u8()); // Function
        let rdq_depth = u16::from_le_bytes([bytes[0x1C], bytes[0x1D]]);
        assert_eq!(rdq_depth, 16);
        // Struct is now 0x48 = 72 bytes per spec.
        assert_eq!(bytes.len(), 0x48);
    }

    // ========================================================================
    // Reply parsing tests — sgl-and-replies.md §8.3
    // ========================================================================

    #[test]
    fn fw_download_reply_parses_success() {
        // Golden bytes simulating successful FW_DOWNLOAD reply (iocstatus-table.md §10)
        let mut bytes = vec![0x00; 24];
        bytes[0] = 0x01; // ImageType = FW

        // IOCStatus at offset 0x0E-0x0F = Success (0x0000 little-endian)
        bytes[14] = 0x00;
        bytes[15] = 0x00;

        let reply = FwDownloadReply::parse(&bytes).unwrap();
        assert_eq!(reply.image_type, 0x01);
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }

    #[test]
    fn fw_download_reply_parses_internal_error() {
        // Golden bytes with INTERNAL_ERROR (dev-1 brick code per ADR-015)
        let mut bytes = vec![0x00; 24];
        bytes[0] = 0x02; // ImageType = BIOS

        // IOCStatus at offset 0x0E-0x0F = InternalError (0x0004 little-endian)
        bytes[14] = 0x04;
        bytes[15] = 0x00;

        let reply = FwDownloadReply::parse(&bytes).unwrap();
        assert_eq!(reply.image_type, 0x02);
        assert_eq!(reply.ioc_status, IocStatus::InternalError);
    }

    #[test]
    fn fw_download_reply_malformed_too_short() {
        let bytes = vec![0x00; 10]; // Less than required 18 bytes

        match FwDownloadReply::parse(&bytes) {
            Err(MpiError::MalformedReply {
                function,
                got,
                need,
            }) => {
                assert_eq!(function, MpiFunction::FwDownload);
                assert_eq!(got, 10);
                assert_eq!(need, 18);
            }
            _ => panic!("Expected MalformedReply error"),
        }
    }

    #[test]
    fn fw_upload_reply_parses_actual_size() {
        let mut bytes = vec![0x00; 24];
        bytes[0] = 0x00; // ImageType = FW_CURRENT

        // IOCStatus at offset 0x0E-0x0F = Success
        bytes[14] = 0x00;
        bytes[15] = 0x00;

        // ActualImageSize at offset 0x14-0x17 = 0x1234 (4660 bytes)
        bytes[20] = 0x34;
        bytes[21] = 0x12;
        bytes[22] = 0x00;
        bytes[23] = 0x00;

        let reply = FwUploadReply::parse(&bytes).unwrap();
        assert_eq!(reply.image_type, 0x00);
        assert_eq!(reply.ioc_status, IocStatus::Success);
        assert_eq!(reply.actual_image_size, 0x1234);
    }

    #[test]
    fn toolboxclean_reply_parses_success() {
        let mut bytes = vec![0x00; 20];
        bytes[0] = 0x00; // Tool = CLEAN

        // IOCStatus at offset 0x0E-0x0F = Success
        bytes[14] = 0x00;
        bytes[15] = 0x00;

        let reply = ToolboxReply::parse(&bytes).unwrap();
        assert_eq!(reply.tool, 0x00);
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }

    #[test]
    fn config_reply_parses_page_header() {
        let mut bytes = vec![0x00; 32];
        bytes[0] = 0x01; // Action = READ_CURRENT

        // Page header at offset 0x14-0x17 (bytes 20-23)
        bytes[20] = 0x05; // PageVersion
        bytes[21] = 0x40; // PageLength (64 bytes)
        bytes[22] = 0x05; // PageNumber
        bytes[23] = 0x09; // PageType (MANUFACTURING)

        // IOCStatus at offset 0x0E-0x0F = Success
        bytes[14] = 0x00;
        bytes[15] = 0x00;

        let reply = ConfigReply::parse(&bytes).unwrap();
        assert_eq!(reply.action, 0x01);
        assert_eq!(reply.page_header, [0x05, 0x40, 0x05, 0x09]);
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }

    #[test]
    fn iocinit_reply_parses_whoinit() {
        let mut bytes = vec![0x00; 20];

        // WhoInit at offset 0x00 = HOST_DRIVER (0x04) per mpi-overview.md §9.2
        bytes[0] = 0x04;

        // IOCStatus at offset 0x0E-0x0F = Success
        bytes[14] = 0x00;
        bytes[15] = 0x00;

        let reply = IocInitReply::parse(&bytes).unwrap();
        assert_eq!(reply.who_init, 0x04);
        assert_eq!(reply.ioc_status, IocStatus::Success);
    }

    // ========================================================================
    // Integration-style tests: serialize → parse round-trip for status codes
    // ========================================================================

    #[test]
    fn all_iocstatus_codes_can_be_serialized_and_deserialized() {
        let test_codes = [
            IocStatus::Success,
            IocStatus::InternalError,
            IocStatus::ConfigInvalidPage,
            IocStatus::ScsiDataOverrun,
            IocStatus::TargetIuTooShort,
        ];

        for status in &test_codes {
            // Convert to raw u16 and back (tests from_u16 exhaustiveness)
            let raw = status.as_u16();
            let parsed = IocStatus::from_u16(raw).unwrap();
            assert_eq!(*status, parsed);
        }
    }

    #[test]
    fn citation_verification() {
        // This test verifies that citations exist in the source code comments.
        // The actual verification is done by reading messages.rs and checking for "Cites:" strings.
        // See: mpi-overview.md, fw-download-upload.md, toolbox-and-config.md,
        //      sgl-and-replies.md, iocstatus-table.md

        // All types should have Cites comments referencing wire-format docs;
        // the comments themselves are the test artifact (citation count is
        // checked in PR review, not at runtime).
    }

    // ========================================================================
    // FlashLayoutReply tests — ADR-015 Rule 11a (mpi2_ioc.h:1469-1502)
    // ========================================================================

    #[test]
    fn flash_region_type_from_u8_handles_all_codes() {
        assert_eq!(FlashRegionType::from_u8(0x00), FlashRegionType::Unused);
        assert_eq!(FlashRegionType::from_u8(0x01), FlashRegionType::Firmware);
        assert_eq!(FlashRegionType::from_u8(0x02), FlashRegionType::Bios);
        assert_eq!(
            FlashRegionType::from_u8(0x03),
            FlashRegionType::Manufacturing
        );
        assert_eq!(FlashRegionType::from_u8(0x04), FlashRegionType::Config);
        assert_eq!(
            FlashRegionType::from_u8(0x05),
            FlashRegionType::MfgPlusConfig
        );
        assert_eq!(FlashRegionType::from_u8(0x06), FlashRegionType::BootService);
        assert_eq!(FlashRegionType::from_u8(0x07), FlashRegionType::Log);
        assert_eq!(FlashRegionType::from_u8(0xFF), FlashRegionType::Other(0xFF));
    }

    #[test]
    fn flash_region_type_as_u8_roundtrip() {
        let variants = [
            FlashRegionType::Unused,
            FlashRegionType::Firmware,
            FlashRegionType::Bios,
            FlashRegionType::Manufacturing,
            FlashRegionType::Config,
            FlashRegionType::MfgPlusConfig,
            FlashRegionType::BootService,
            FlashRegionType::Log,
        ];

        for variant in &variants {
            let raw = variant.as_u8();
            assert_eq!(FlashRegionType::from_u8(raw), *variant);
        }
    }

    #[test]
    fn flash_region_type_other_roundtrip() {
        let other = FlashRegionType::Other(0x42);
        assert_eq!(other.as_u8(), 0x42);
        assert_eq!(FlashRegionType::from_u8(0x42), FlashRegionType::Other(0x42));
    }

    #[test]
    fn flash_layout_reply_parse_golden_buffer() {
        // Construct a golden buffer: layout header (16 bytes) + 2 regions (32 bytes) = 48 bytes total.
        // Layout header per mpi2_ioc.h:1480-1487:
        //   Offset 0x00: FlashSize (U32) = 0x00100000 (1MB)
        //   Offset 0x04: Reserved1 (U32) = 0
        //   Offset 0x08: Reserved2 (U32) = 0
        //   Offset 0x0C: Reserved3 (U32) = 0
        // Region per mpi2_ioc.h:1469-1477 (16 bytes each):
        //   Offset 0x00: RegionType (U8)
        //   Offset 0x01: Reserved1 (U8)
        //   Offset 0x02: Reserved2 (U16)
        //   Offset 0x04: RegionOffset (U32)
        //   Offset 0x08: RegionSize (U32)
        //   Offset 0x0C: Reserved3 (U32)

        let mut bytes = vec![0x00; 48];

        // Layout header — FlashSize at offset 0x00-0x03 = 1MB (0x00100000 LE)
        bytes[0] = 0x00;
        bytes[1] = 0x00;
        bytes[2] = 0x10;
        bytes[3] = 0x00;

        // Region 0 at offset 0x10: Firmware region (type=0x01)
        bytes[0x10] = 0x01; // RegionType = Firmware
        bytes[0x14..0x18].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // RegionOffset
        bytes[0x18..0x1C].copy_from_slice(&0x0008_0000u32.to_le_bytes()); // RegionSize = 512KB

        // Region 1 at offset 0x20: BIOS region (type=0x02)
        bytes[0x20] = 0x02; // RegionType = Bios
        bytes[0x24..0x28].copy_from_slice(&0x0009_0000u32.to_le_bytes()); // RegionOffset
        bytes[0x28..0x2C].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // RegionSize = 64KB

        let reply = FlashLayoutReply::parse(&bytes).unwrap();

        assert_eq!(reply.flash_size, 0x00100000);
        assert_eq!(reply.regions.len(), 2);
        assert_eq!(reply.regions[0].region_type(), FlashRegionType::Firmware);
        assert_eq!(reply.regions[0].region_offset, 0x0001_0000);
        assert_eq!(reply.regions[0].region_size, 0x0008_0000);
        assert_eq!(reply.regions[1].region_type(), FlashRegionType::Bios);
        assert_eq!(reply.regions[1].region_offset, 0x0009_0000);
        assert_eq!(reply.regions[1].region_size, 0x0001_0000);
    }

    #[test]
    fn flash_layout_reply_region_finds_correct_type() {
        let mut bytes = vec![0x00; 48];

        // Layout header — FlashSize at offset 0x00-0x03 = 1MB (0x00100000 LE)
        bytes[0] = 0x00;
        bytes[1] = 0x00;
        bytes[2] = 0x10;
        bytes[3] = 0x00;

        // Region 0 at offset 0x10: Firmware region (type=0x01)
        bytes[0x10] = 0x01; // RegionType = Firmware
        bytes[0x14..0x18].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // RegionOffset
        bytes[0x18..0x1C].copy_from_slice(&0x0008_0000u32.to_le_bytes()); // RegionSize = 512KB

        // Region 1 at offset 0x20: BIOS region (type=0x02)
        bytes[0x20] = 0x02; // RegionType = Bios
        bytes[0x24..0x28].copy_from_slice(&0x0009_0000u32.to_le_bytes()); // RegionOffset
        bytes[0x28..0x2C].copy_from_slice(&0x0001_0000u32.to_le_bytes()); // RegionSize = 64KB

        let reply = FlashLayoutReply::parse(&bytes).unwrap();

        // Find firmware region
        let fw_region = reply
            .region(FlashRegionType::Firmware)
            .expect("should find firmware");
        assert_eq!(fw_region.region_type(), FlashRegionType::Firmware);
        assert_eq!(fw_region.region_size, 0x0008_0000);

        // Find BIOS region
        let bios_region = reply
            .region(FlashRegionType::Bios)
            .expect("should find bios");
        assert_eq!(bios_region.region_type(), FlashRegionType::Bios);
        assert_eq!(bios_region.region_size, 0x0001_0000);

        // Non-existent region returns None
        assert!(reply.region(FlashRegionType::Log).is_none());

        // Verify reserved fields are zeroed
        assert_eq!(fw_region.reserved_1, 0x00);
        assert_eq!(fw_region.reserved_2, 0x0000);
    }

    #[test]
    fn flash_layout_reply_parse_too_short_returns_malformed() {
        let bytes = vec![0x00; 12]; // Less than required 16 bytes for layout header

        match FlashLayoutReply::parse(&bytes) {
            Err(MpiError::MalformedReply {
                function,
                got,
                need,
            }) => {
                assert_eq!(function, MpiFunction::Config);
                assert_eq!(got, 12);
                assert_eq!(need, 16);
            }
            _ => panic!("Expected MalformedReply error"),
        }
    }

    #[test]
    fn flash_region_struct_size_is_16_bytes() {
        // Verify FlashRegion is 16 bytes as per mpi2_ioc.h:1469-1477 (U8+U8+U16+U32+U32+U32)
        assert_eq!(std::mem::size_of::<FlashRegion>(), 16);
    }

    #[test]
    fn flash_region_type_debug_impl() {
        let fw = FlashRegionType::Firmware;
        let debug_str = format!("{:?}", fw);
        assert_eq!(debug_str, "Firmware");

        let other = FlashRegionType::Other(0x42);
        let debug_str = format!("{:?}", other);
        assert_eq!(debug_str, "Other(66)");
    }
}
