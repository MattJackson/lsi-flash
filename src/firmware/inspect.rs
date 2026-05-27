//! Firmware header parsing implementation.
//! Cites: references/upstream/lsiutil/lsi/mpi2_ioc.h (lines 1314-1362, 1365-1409)

use thiserror::Error;

/// Signature magic value for valid firmware header - byte 0 is 0xEA.
/// Cites mpi2_ioc.h:1367.
pub const MPI2_FW_HEADER_SIGNATURE: u32 = 0xEA000000;

/// Signature0 magic value for valid firmware header.
/// Cites mpi2_ioc.h:1371.
pub const MPI2_FW_HEADER_SIGNATURE0: u32 = 0x5AFAA55A;

/// Signature1 magic value for valid firmware header.
/// Cites mpi2_ioc.h:1375.
pub const MPI2_FW_HEADER_SIGNATURE1: u32 = 0xA55AFAA5;

/// Signature2 magic value for valid firmware header.
/// Cites mpi2_ioc.h:1379.
pub const MPI2_FW_HEADER_SIGNATURE2: u32 = 0x5AA55AFA;

/// Firmware header structure offset constants from mpi2_ioc.h.
/// Cites mpi2_ioc.h:1365-1408.
pub const FW_HEADER_OFFSET_SIGNATURE: usize = 0x00; // 4 bytes
pub const FW_HEADER_OFFSET_SIGNATURE0: usize = 0x04; // 4 bytes
pub const FW_HEADER_OFFSET_SIGNATURE1: usize = 0x08; // 4 bytes
pub const FW_HEADER_OFFSET_SIGNATURE2: usize = 0x0C; // 4 bytes
pub const FW_HEADER_OFFSET_MPI_VERSION: usize = 0x10; // union (8 bytes)
pub const FW_HEADER_OFFSET_FW_VERSION: usize = 0x14; // union (8 bytes)
pub const FW_HEADER_OFFSET_IMAGE_SIZE: usize = 0x2C; // 4 bytes
pub const FW_HEADER_OFFSET_VENDOR_ID: usize = 0x20; // 2 bytes
pub const FW_HEADER_OFFSET_DEVICE_ID: usize = 0x22; // 2 bytes
pub const FW_HEADER_OFFSET_PROTOCOL_FLAGS: usize = 0x24; // 2 bytes
pub const FW_HEADER_OFFSET_IOC_CAPABILITIES: usize = 0x28; // 4 bytes
pub const FW_HEADER_OFFSET_CHECKSUM: usize = 0x34; // 4 bytes

/// Total firmware header size from mpi2_ioc.h.
/// Cites mpi2_ioc.h:1409.
pub const FW_HEADER_SIZE: usize = 0x100;

/// MPI Fusion-MPT firmware header. Cites references/upstream/lsiutil/lsi/mpi2_ioc.h:1314-1362.
#[derive(Debug, Clone)]
pub struct FwHeader {
    pub signature: u32,        // 0xEA000000 (little-endian)
    pub signature0: u32,       // 0x5AFAA55A (little-endian)
    pub signature1: u32,       // 0xA55AFAA5 (little-endian)
    pub signature2: u32,       // 0x5AA55AFA (little-endian)
    pub vendor_id: u16,        // PCI Vendor ID (0x1000 = LSI/Broadcom)
    pub device_id: u16,        // PCI Device ID
    pub protocol_flags: u16,   // Protocol flags
    pub ioc_capabilities: u32, // IOC capabilities
    pub image_size: u32,       // Total firmware size in bytes
    pub checksum: u32,         // Firmware checksum
}

/// Error type for firmware operations. Cites thiserror usage from scoping doc §1.
#[derive(Error, Debug)]
pub enum FwError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Firmware too short (got {0} bytes, need at least 32)")]
    TooShort(usize),

    #[error("Invalid firmware signature (expected 0x{expected:08X}, got 0x{got:08X})")]
    InvalidSignature { expected: u32, got: u32 },

    #[error("Invalid firmware signature0 (expected 0x{expected:08X}, got 0x{got:08X})")]
    InvalidSignature0 { expected: u32, got: u32 },

    #[error("Invalid firmware signature1 (expected 0x{expected:08X}, got 0x{got:08X})")]
    InvalidSignature1 { expected: u32, got: u32 },

    #[error("Invalid firmware signature2 (expected 0x{expected:08X}, got 0x{got:08X})")]
    InvalidSignature2 { expected: u32, got: u32 },
}

/// Parse firmware header from binary. Cites mpi2_ioc.h + golden file test.
pub fn parse_fw_header(data: &[u8]) -> Result<FwHeader, FwError> {
    if data.len() < 0x14 {
        return Err(FwError::TooShort(data.len()));
    }

    let signature = u32::from_le_bytes(
        data[FW_HEADER_OFFSET_SIGNATURE..FW_HEADER_OFFSET_SIGNATURE + 4]
            .try_into()
            .unwrap(),
    );
    if signature != MPI2_FW_HEADER_SIGNATURE {
        return Err(FwError::InvalidSignature {
            expected: MPI2_FW_HEADER_SIGNATURE,
            got: signature,
        });
    }

    let signature0 = u32::from_le_bytes(
        data[FW_HEADER_OFFSET_SIGNATURE0..FW_HEADER_OFFSET_SIGNATURE0 + 4]
            .try_into()
            .unwrap(),
    );
    if signature0 != MPI2_FW_HEADER_SIGNATURE0 {
        return Err(FwError::InvalidSignature0 {
            expected: MPI2_FW_HEADER_SIGNATURE0,
            got: signature0,
        });
    }

    let signature1 = u32::from_le_bytes(
        data[FW_HEADER_OFFSET_SIGNATURE1..FW_HEADER_OFFSET_SIGNATURE1 + 4]
            .try_into()
            .unwrap(),
    );
    if signature1 != MPI2_FW_HEADER_SIGNATURE1 {
        return Err(FwError::InvalidSignature1 {
            expected: MPI2_FW_HEADER_SIGNATURE1,
            got: signature1,
        });
    }

    let signature2 = u32::from_le_bytes(
        data[FW_HEADER_OFFSET_SIGNATURE2..FW_HEADER_OFFSET_SIGNATURE2 + 4]
            .try_into()
            .unwrap(),
    );
    if signature2 != MPI2_FW_HEADER_SIGNATURE2 {
        return Err(FwError::InvalidSignature2 {
            expected: MPI2_FW_HEADER_SIGNATURE2,
            got: signature2,
        });
    }

    Ok(FwHeader {
        signature,
        signature0,
        signature1,
        signature2,
        vendor_id: u16::from_le_bytes(
            data[FW_HEADER_OFFSET_VENDOR_ID..FW_HEADER_OFFSET_VENDOR_ID + 2]
                .try_into()
                .unwrap(),
        ),
        device_id: u16::from_le_bytes(
            data[FW_HEADER_OFFSET_DEVICE_ID..FW_HEADER_OFFSET_DEVICE_ID + 2]
                .try_into()
                .unwrap(),
        ),
        protocol_flags: u16::from_le_bytes(
            data[FW_HEADER_OFFSET_PROTOCOL_FLAGS..FW_HEADER_OFFSET_PROTOCOL_FLAGS + 2]
                .try_into()
                .unwrap(),
        ),
        ioc_capabilities: u32::from_le_bytes(
            data[FW_HEADER_OFFSET_IOC_CAPABILITIES..FW_HEADER_OFFSET_IOC_CAPABILITIES + 4]
                .try_into()
                .unwrap(),
        ),
        image_size: u32::from_le_bytes(
            data[FW_HEADER_OFFSET_IMAGE_SIZE..FW_HEADER_OFFSET_IMAGE_SIZE + 4]
                .try_into()
                .unwrap(),
        ),
        checksum: u32::from_le_bytes(
            data[FW_HEADER_OFFSET_CHECKSUM..FW_HEADER_OFFSET_CHECKSUM + 4]
                .try_into()
                .unwrap(),
        ),
    })
}

/// Inspect firmware file and print human-readable info.
pub fn inspect_firmware(path: &str) -> Result<FwHeader, FwError> {
    let data = std::fs::read(path)?;
    let header = parse_fw_header(&data)?;

    println!("Firmware Header ({} bytes):", data.len());
    println!("  Signature:        0x{:08X} ('_FWI')", header.signature);
    println!("  Signature0:       0x{:08X}", header.signature0);
    println!("  Signature1:       0x{:08X}", header.signature1);
    println!("  Signature2:       0x{:08X}", header.signature2);
    println!(
        "  Version Name:     {}",
        String::from_utf8_lossy(&data[0x68..0x88])
    );
    println!("  Vendor ID:        0x{:04X}", header.vendor_id);
    println!("  Device ID:        0x{:04X}", header.device_id);
    println!("  Protocol Flags:   0x{:04X}", header.protocol_flags);
    println!("  IOC Capabilities: 0x{:08X}", header.ioc_capabilities);
    println!("  Image Size:       {} bytes", header.image_size);
    println!("  Checksum:         0x{:08X}", header.checksum);

    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_2118it_bin() {
        // Fixture lives in the sibling lsi-flash-notes repo (not shipped here);
        // skip cleanly when it isn't present (CI runners, contributors, etc.).
        let fixture =
            "/Users/mjackson/Developer/lsi-flash-notes/09-research-archive/upstream/lsi_sas_hba_crossflash_guide/2118it.bin";
        let Ok(data) = std::fs::read(fixture) else {
            eprintln!("skipping: fixture {fixture} not present");
            return;
        };
        let header = parse_fw_header(&data).unwrap();

        assert_eq!(header.signature, MPI2_FW_HEADER_SIGNATURE);
        assert_eq!(header.signature0, MPI2_FW_HEADER_SIGNATURE0);
        assert_eq!(header.signature1, MPI2_FW_HEADER_SIGNATURE1);
        assert_eq!(header.signature2, MPI2_FW_HEADER_SIGNATURE2);

        // LSI vendor ID (0x1000) - from mpi2_ioc.h device definitions
        assert_eq!(header.vendor_id, 0x1000);

        println!("Parsed 2118it.bin header successfully:");
        println!("  Vendor ID:       0x{:04X}", header.vendor_id);
        println!("  Device ID:       0x{:04X}", header.device_id);
        println!("  Image Size:      {} bytes", header.image_size);
        println!("  Checksum:        0x{:08X}", header.checksum);
    }

    #[test]
    fn test_parse_too_short() {
        let data = vec![0u8; 16];
        let result = parse_fw_header(&data);

        assert!(result.is_err());
        match result.unwrap_err() {
            FwError::TooShort(len) => assert_eq!(len, 16),
            _ => panic!("Expected TooShort error"),
        }
    }

    #[test]
    fn test_parse_invalid_signature() {
        let mut data = vec![0u8; 32];
        // Set wrong signature (not 0xEA000000)
        data[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());

        let result = parse_fw_header(&data);

        assert!(result.is_err());
        match result.unwrap_err() {
            FwError::InvalidSignature { expected, got } => {
                assert_eq!(expected, MPI2_FW_HEADER_SIGNATURE);
                assert_eq!(got, 0x12345678);
            }
            _ => panic!("Expected InvalidSignature error"),
        }
    }

    #[test]
    fn test_parse_invalid_signature0() {
        let mut data = vec![0u8; 32];
        // Set correct signature, wrong signature0
        data[0..4].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE.to_le_bytes());
        data[4..8].copy_from_slice(&0x12345678u32.to_le_bytes());

        let result = parse_fw_header(&data);

        assert!(result.is_err());
        match result.unwrap_err() {
            FwError::InvalidSignature0 { expected, got } => {
                assert_eq!(expected, MPI2_FW_HEADER_SIGNATURE0);
                assert_eq!(got, 0x12345678);
            }
            _ => panic!("Expected InvalidSignature0 error"),
        }
    }

    #[test]
    fn test_parse_invalid_signature1() {
        let mut data = vec![0u8; 32];
        // Set correct signature and signature0, wrong signature1
        data[0..4].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE.to_le_bytes());
        data[4..8].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE0.to_le_bytes());
        data[8..12].copy_from_slice(&0x12345678u32.to_le_bytes());

        let result = parse_fw_header(&data);

        assert!(result.is_err());
        match result.unwrap_err() {
            FwError::InvalidSignature1 { expected, got } => {
                assert_eq!(expected, MPI2_FW_HEADER_SIGNATURE1);
                assert_eq!(got, 0x12345678);
            }
            _ => panic!("Expected InvalidSignature1 error"),
        }
    }

    #[test]
    fn test_parse_invalid_signature2() {
        let mut data = vec![0u8; 32];
        // Set correct signature, signature0, signature1, wrong signature2
        data[0..4].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE.to_le_bytes());
        data[4..8].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE0.to_le_bytes());
        data[8..12].copy_from_slice(&MPI2_FW_HEADER_SIGNATURE1.to_le_bytes());
        data[12..16].copy_from_slice(&0x12345678u32.to_le_bytes());

        let result = parse_fw_header(&data);

        assert!(result.is_err());
        match result.unwrap_err() {
            FwError::InvalidSignature2 { expected, got } => {
                assert_eq!(expected, MPI2_FW_HEADER_SIGNATURE2);
                assert_eq!(got, 0x12345678);
            }
            _ => panic!("Expected InvalidSignature2 error"),
        }
    }

    #[test]
    fn test_fw_header_size() {
        // Verify header size matches mpi2_ioc.h:1409
        assert_eq!(FW_HEADER_SIZE, 0x100);
    }
}
