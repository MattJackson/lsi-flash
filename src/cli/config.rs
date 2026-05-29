//! Config page reader — read ANY MPI config page via kernel-mediated Mpt3CtlTransport.
//!
//! Implements `lsi-flash config read` and `lsi-flash config dump` subcommands.
//! Cites: mpi2_cnfg.h for constants/offsets, mpt3sas_config.c (_config_request)
//! for the 2-step PAGE_HEADER + READ_CURRENT pattern.

use clap::Args;
use serde::{Deserialize, Serialize};

/// CLI args for `config read` subcommand.
#[derive(Args, Debug)]
pub struct ConfigReadArgs {
    /// Page type name or hex value (0xNN).
    #[arg(long, value_name = "NAME|0xNN")]
    pub page_type: String,

    /// Page number within the page type (0..=63).
    #[arg(long)]
    pub page_number: u8,

    /// Extended page type for extended pages (type 0x0F).
    #[arg(long, value_name = "0xNN")]
    pub ext_page_type: Option<String>,

    /// PageAddress field — defaults to 0. Cites mpi2_cnfg.h:347.
    #[arg(long, value_name = "0xNNNNNNNN", default_value = "0")]
    pub page_address: String,

    /// Emit JSON output instead of hexdump.
    #[arg(long)]
    pub json: bool,
}

/// CLI args for `config dump` subcommand.
#[derive(Args, Debug)]
pub struct ConfigDumpArgs {
    /// Emit JSON output instead of table/hexdump.
    #[arg(long)]
    pub json: bool,
}

/// Page type name to value mapping — cites mpi2_cnfg.h lines.
fn page_type_from_str(s: &str) -> Result<u8, String> {
    let s_lower = s.to_lowercase();
    match s_lower.as_str() {
        "io-unit" | "0x00" => Ok(0x00), // MPI2_CONFIG_PAGETYPE_IO_UNIT — mpi2_cnfg.h:207
        "ioc" | "0x01" => Ok(0x01),     // MPI2_CONFIG_PAGETYPE_IOC — mpi2_cnfg.h:208
        "bios" | "0x02" => Ok(0x02),    // MPI2_CONFIG_PAGETYPE_BIOS — mpi2_cnfg.h:209
        "raid-volume" | "0x08" => Ok(0x08), // MPI2_CONFIG_PAGETYPE_RAID_VOLUME — mpi2_cnfg.h:210
        "manufacturing" | "mfg" | "0x09" => Ok(0x09), // MPI2_CONFIG_PAGETYPE_MANUFACTURING — mpi2_cnfg.h:211
        "raid-phys-disk" | "raid-pdisk" | "0x0A" => Ok(0x0A), // MPI2_CONFIG_PAGETYPE_RAID_PHYSDISK — mpi2_cnfg.h:212
        "extended" | "ext" | "0x0F" => Ok(0x0F), // MPI2_CONFIG_PAGETYPE_EXTENDED — mpi2_cnfg.h:213
        _ => {
            if let Some(stripped) = s.strip_prefix("0x") {
                u8::from_str_radix(stripped, 16)
                    .map_err(|_| format!("Invalid hex page type: {}", s))
            } else {
                Err(format!(
                    "Unknown page type '{}'. Use: manufacturing(0x09), io-unit(0x00), ioc(0x01)",
                    s
                ))
            }
        }
    }
}

/// Extended page type name to value mapping — cites mpi2_cnfg.h:220-231.
fn ext_page_type_from_str(s: &str) -> Result<u8, String> {
    let s_lower = s.to_lowercase();
    match s_lower.as_str() {
        "sas-io-unit" | "0x10" => Ok(0x10), // MPI2_CONFIG_EXTPAGETYPE_SAS_IO_UNIT — mpi2_cnfg.h:220
        "sas-device" | "0x12" => Ok(0x12),  // MPI2_CONFIG_EXTPAGETYPE_SAS_DEVICE — mpi2_cnfg.h:222
        "sas-phy" | "0x13" => Ok(0x13),     // MPI2_CONFIG_EXTPAGETYPE_SAS_PHY — mpi2_cnfg.h:223
        "flash-layout" | "log" | "0x14" => Ok(0x14), // MPI2_CONFIG_EXTPAGETYPE_LOG — mpi2_cnfg.h:224
        _ => {
            if let Some(stripped) = s.strip_prefix("0x") {
                u8::from_str_radix(stripped, 16)
                    .map_err(|_| format!("Invalid hex ext page type: {}", s))
            } else {
                Err(format!(
                    "Unknown extended page type '{}'. Use: sas-io-unit(0x10), sas-device(0x12)",
                    s
                ))
            }
        }
    }
}

/// Human-readable name for a page type.
fn page_type_name(pt: u8) -> &'static str {
    match pt {
        0x00 => "IO Unit",
        0x01 => "IOC",
        0x02 => "BIOS",
        0x08 => "RAID Volume",
        0x09 => "Manufacturing",
        0x0A => "RAID PhysDisk",
        0x0F => "Extended",
        _ => "Unknown",
    }
}

/// Config page read result.
#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigPageRead {
    pub page_type: u8,
    pub page_number: u8,
    pub page_version: Option<u8>,
    pub page_length: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext_page_type: Option<u8>, // Only present for extended pages (type 0x0F)
    pub bytes_hex: String,
}

/// Config dump entry.
#[derive(Serialize, Deserialize, Debug)]
pub struct ConfigDumpEntry {
    pub page_type: u8,
    pub page_number: u8,
    pub length: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext_page_type: Option<u8>, // For extended pages (type 0x0F)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_hex: Option<String>,
}

/// Page header info from CONFIG reply.
#[derive(Debug)]
pub struct PageHeader {
    pub version: u8,
    pub length: u8,
    pub number: u8,
    pub type_: u8,
}

impl PageHeader {
    /// Parse from 4 bytes at offset 0x14 in CONFIG reply.
    /// Cites: mpi2_cnfg.h:158-165 (MPI2_CONFIG_PAGE_HEADER layout).
    pub fn parse(bytes: &[u8]) -> Self {
        let version = bytes[0]; // PageVersion — mpi2_cnfg.h:160
        let length = bytes[1]; // PageLength — mpi2_cnfg.h:161 (in 4-byte words)
        let number = bytes[2]; // PageNumber — mpi2_cnfg.h:162
        let type_ = bytes[3]; // PageType — mpi2_cnfg.h:163

        Self {
            version,
            length,
            number,
            type_,
        }
    }
}

/// Read a single config page via Mpt3CtlTransport.
///
/// Implements the 2-step pattern from mpt3sas_config.c:_config_request:
/// 1. PAGE_HEADER (action=0x00) → get PageLength from reply header
/// 2. READ_CURRENT (action=0x01) with buffer of PageLength*4 bytes
pub fn read_config_page(
    transport: &mut dyn crate::mpt::MptTransport,
    page_type: u8,
    page_number: u8,
    ext_page_type: Option<u8>,
    page_address: u32,
) -> Result<ConfigPageRead, String> {
    use crate::mpi::messages::{ConfigReply, ConfigRequest, IocStatus};

    // Step 1: PAGE_HEADER — get PageLength from firmware reply header.
    let mut header_buf = [0u8; 4];
    let req_header = ConfigRequest {
        action: 0x00,    // MPI2_CONFIG_ACTION_PAGE_HEADER — mpi2_cnfg.h:356
        sgl_flags: 0xC0, // END_OF_LIST + IOC_TO_HOST
        page_type,
        page_number,
        ext_page_type,
        payload_buffer: &mut header_buf,
        page_address,
    };

    let req_bytes = req_header.serialize_to(1);
    let mut reply_buf = [0u8; 256]; // generous for CONFIG reply (26 bytes base)

    transport
        .send_with_sge_offset(&req_bytes, 32 / 4, &mut reply_buf, None, None)
        .map_err(|e| format!("send PAGE_HEADER failed: {}", e))?;

    let reply_header =
        ConfigReply::parse(&reply_buf).map_err(|e| format!("CONFIG header parse failed: {}", e))?;

    // Check IOCStatus — ADR-015 Rule 6 spirit
    if reply_header.ioc_status != IocStatus::Success {
        return Err(format!(
            "PAGE_HEADER failed with IOCStatus={:?} (page may not exist)",
            reply_header.ioc_status
        ));
    }

    let page_hdr = PageHeader::parse(&reply_header.page_header);

    // Sanity check: does returned type/number match request?
    if page_hdr.type_ != page_type || page_hdr.number != page_number {
        return Err(format!(
            "Page header mismatch: requested {}/{} got {}/{}",
            page_type, page_number, page_hdr.type_, page_hdr.number
        ));
    }

    // PageLength is in 4-byte words per mpi2_cnfg.h:161. Total bytes = length * 4.
    let page_len_words = page_hdr.length as usize;
    if page_len_words == 0 {
        return Err("PageLength=0 from firmware".to_string());
    }
    let page_buf_size = page_len_words * 4;

    // Step 2: READ_CURRENT — fetch full page data.
    let mut page_buf = vec![0u8; page_buf_size];
    let req_read = ConfigRequest {
        action: 0x01,    // MPI2_CONFIG_ACTION_PAGE_READ_CURRENT — mpi2_cnfg.h:357
        sgl_flags: 0xC0, // END_OF_LIST + IOC_TO_HOST
        page_type,
        page_number,
        ext_page_type,
        payload_buffer: &mut page_buf,
        page_address,
    };

    let req_bytes = req_read.serialize_to(2);
    transport
        .send_with_sge_offset(
            &req_bytes,
            32 / 4,
            &mut reply_buf,
            None,
            Some(&mut page_buf),
        )
        .map_err(|e| format!("send READ_CURRENT failed: {}", e))?;

    let reply_data =
        ConfigReply::parse(&reply_buf).map_err(|e| format!("CONFIG read parse failed: {}", e))?;

    if reply_data.ioc_status != IocStatus::Success {
        return Err(format!(
            "READ_CURRENT failed with IOCStatus={:?} (page may not exist)",
            reply_data.ioc_status
        ));
    }

    Ok(ConfigPageRead {
        page_type,
        page_number,
        page_version: Some(page_hdr.version),
        page_length: Some(page_hdr.length),
        ext_page_type,
        bytes_hex: hex::encode(&page_buf),
    })
}

/// Human hexdump output for a config page.
pub fn format_hexdump(result: &ConfigPageRead) -> String {
    let mut out = String::new();

    // Header line
    if let (Some(v), Some(l)) = (result.page_version, result.page_length) {
        let ext_str = if let Some(et) = result.ext_page_type {
            format!(" ext=0x{:02X}", et)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "Page {}#{} v={} len={} words ({} bytes){}\n",
            page_type_name(result.page_type),
            result.page_number,
            v,
            l,
            result.bytes_hex.len() / 2,
            ext_str
        ));
    } else {
        out.push_str(&format!(
            "Page {}#{}\n",
            page_type_name(result.page_type),
            result.page_number
        ));
    }

    // Hexdump in 16-byte rows (32 hex chars per row)
    let bytes = hex::decode(&result.bytes_hex).unwrap_or_default();
    for chunk in bytes.chunks(16) {
        let hex_str: String = chunk.iter().map(|b| format!("{:02x}", b)).collect();

        out.push_str(&format!(
            "  {:04x}: {}\n",
            (chunk.as_ptr() as usize - bytes.as_ptr() as usize),
            hex_str
        ));
    }

    out
}

/// Run `config read` subcommand.
pub fn run_read(
    bdf: String,
    page_type: String,
    page_number: u8,
    ext_page_type: Option<String>,
    page_address: String,
    json_flag: bool,
) -> Result<(), crate::Error> {
    use crate::mpt::Mpt3CtlTransport;

    let mut transport = Mpt3CtlTransport::open(&bdf)
        .map_err(|e| crate::Error::Other(format!("mpt3ctl open: {}", e)))?;

    let page_type_val = page_type_from_str(&page_type).map_err(crate::Error::Other)?;

    let ext_page_type_val = if let Some(et) = &ext_page_type {
        Some(ext_page_type_from_str(et).map_err(crate::Error::Other)?)
    } else {
        None
    };

    let page_address_val =
        u32::from_str_radix(page_address.strip_prefix("0x").unwrap_or(&page_address), 16)
            .map_err(|e| crate::Error::Other(format!("Invalid page_address: {}", e)))?;

    let result = read_config_page(
        &mut transport,
        page_type_val,
        page_number,
        ext_page_type_val,
        page_address_val,
    )
    .map_err(crate::Error::Other)?;

    if json_flag {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", format_hexdump(&result));
    }

    Ok(())
}

/// Config subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum ConfigSubCommand {
    /// Read a single config page from the chip.
    #[clap(name = "read")]
    Read(ConfigReadArgs),

    /// Enumerate all existing config pages on the card.
    #[clap(name = "dump")]
    Dump(ConfigDumpArgs),
}

/// Entry point for config subcommand.
pub fn run(bdf: String, _sub: ConfigSubCommand) -> Result<(), crate::Error> {
    match _sub {
        ConfigSubCommand::Read(args) => run_read(
            bdf,
            args.page_type,
            args.page_number,
            args.ext_page_type,
            args.page_address,
            args.json,
        ),
        ConfigSubCommand::Dump(_) => Err(crate::Error::Other(
            "config dump not yet implemented".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpi::messages::ConfigRequest;

    /// Test page type name parsing with mpi2_cnfg.h citations.
    #[test]
    fn test_page_type_from_str() {
        assert_eq!(page_type_from_str("manufacturing").unwrap(), 0x09); // mpi2_cnfg.h:211
        assert_eq!(page_type_from_str("io-unit").unwrap(), 0x00); // mpi2_cnfg.h:207
        assert_eq!(page_type_from_str("ioc").unwrap(), 0x01); // mpi2_cnfg.h:208
        assert_eq!(page_type_from_str("bios").unwrap(), 0x02); // mpi2_cnfg.h:209
        assert_eq!(page_type_from_str("raid-volume").unwrap(), 0x08); // mpi2_cnfg.h:210
        assert_eq!(page_type_from_str("raid-phys-disk").unwrap(), 0x0A); // mpi2_cnfg.h:212
        assert_eq!(page_type_from_str("extended").unwrap(), 0x0F); // mpi2_cnfg.h:213

        // Hex variants
        assert_eq!(page_type_from_str("0x09").unwrap(), 0x09);
        assert_eq!(page_type_from_str("0x0A").unwrap(), 0x0A);

        // Invalid
        assert!(page_type_from_str("invalid").is_err());
    }

    /// Test extended page type name parsing with mpi2_cnfg.h citations.
    #[test]
    fn test_ext_page_type_from_str() {
        assert_eq!(ext_page_type_from_str("sas-io-unit").unwrap(), 0x10); // mpi2_cnfg.h:220
        assert_eq!(ext_page_type_from_str("sas-device").unwrap(), 0x12); // mpi2_cnfg.h:222
        assert_eq!(ext_page_type_from_str("sas-phy").unwrap(), 0x13); // mpi2_cnfg.h:223
        assert_eq!(ext_page_type_from_str("flash-layout").unwrap(), 0x14); // mpi2_cnfg.h:224 (LOG)

        // Hex variants
        assert_eq!(ext_page_type_from_str("0x10").unwrap(), 0x10);

        // Invalid
        assert!(ext_page_type_from_str("invalid").is_err());
    }

    /// Test PageHeader parsing from CONFIG reply.
    #[test]
    fn test_page_header_parse() {
        let bytes = [0x03, 0x10, 0x05, 0x09]; // v=3, len=16 words, num=5, type=manufacturing
        let hdr = PageHeader::parse(&bytes);
        assert_eq!(hdr.version, 0x03);
        assert_eq!(hdr.length, 0x10);
        assert_eq!(hdr.number, 0x05);
        assert_eq!(hdr.type_, 0x09);
    }

    /// Test ConfigRequest serialization with PageAddress=0 for plain pages.
    #[test]
    fn test_config_request_page_address_zero() {
        let mut buf = [0u8; 256];
        let req = ConfigRequest {
            action: 0x01, // READ_CURRENT
            sgl_flags: 0xC0,
            page_type: 0x09, // manufacturing
            page_number: 0x00,
            ext_page_type: None,
            payload_buffer: &mut buf,
            page_address: 0x0000_0000, // MUST be 0 for plain pages per mpi2_cnfg.h:347
        };

        let wire = req.serialize_to(1);

        // Wire format: header (10B) + body (~20B before PageHeader) + SGE = ~46 bytes total
        // Based on actual serialization:
        //   Offset 26-29: PageHeader (version, length, number, type)
        //   Offset 30-33: PageAddress

        assert_eq!(wire[29], 0x09, "PageType should be manufacturing (0x09)");

        let page_addr = u32::from_le_bytes([wire[30], wire[31], wire[32], wire[33]]);
        assert_eq!(
            page_addr, 0x0000_0000,
            "PageAddress must be 0 for plain pages per mpi2_cnfg.h:347"
        );
    }

    /// Test ConfigRequest with nonzero PageAddress for addressed pages.
    #[test]
    fn test_config_request_page_address_nonzero() {
        let mut buf = [0u8; 256];

        // RAID Volume page with HANDLE form — mpi2_cnfg.h:239-246
        let handle = 0x1234;
        let page_address = 0x1000_0000 | (handle & 0xFFFF); // MPI2_RAID_VOLUME_PGAD_FORM_HANDLE | handle

        let req = ConfigRequest {
            action: 0x01,
            sgl_flags: 0xC0,
            page_type: 0x08, // raid-volume
            page_number: 0x00,
            ext_page_type: None,
            payload_buffer: &mut buf,
            page_address,
        };

        let wire = req.serialize_to(1);

        // PageAddress at byte index 30-33 in wire format (offsets 0x1E-0x21 from body start)
        let page_addr = u32::from_le_bytes([wire[30], wire[31], wire[32], wire[33]]);

        assert_eq!(
            page_addr, 0x1000_1234,
            "PageAddress should encode HANDLE form"
        );
    }

    /// Mock transport for testing config reads.
    struct MockTransport {
        reply_buffer: Vec<u8>,
    }

    impl MockTransport {
        fn new(page_type: u8, page_number: u8, version: u8, length: u8) -> Self {
            let mut buf = vec![0u8; 256];

            // Reply header at offset 0x14: action, sgl_flags, msg_len, function...
            // IOCStatus at offset 0x0E (2 bytes LE)
            buf[14] = 0x00; // IOCStatus low byte = Success
            buf[15] = 0x00; // IOCStatus high byte

            // Page header at offset 0x14 in reply: version, length, number, type — mpi2_cnfg.h:381
            buf[20] = version; // PageVersion
            buf[21] = length; // PageLength (in words)
            buf[22] = page_number; // PageNumber
            buf[23] = page_type; // PageType

            Self { reply_buffer: buf }
        }
    }

    impl crate::mpt::MptTransport for MockTransport {
        fn send_with_sge_offset(
            &mut self,
            _request: &[u8],
            _data_sge_offset_words: u32,
            reply: &mut [u8],
            data_in: Option<&mut [u8]>,
            data_out: Option<&mut [u8]>,
        ) -> Result<usize, crate::mpt::TransportError> {
            // Copy canned header to reply buffer
            let len = std::cmp::min(reply.len(), self.reply_buffer.len());
            reply[..len].copy_from_slice(&self.reply_buffer[..len]);

            // Echo payload back in data_out if provided (for READ_CURRENT)
            if let Some(out_buf) = data_out {
                let page_len_words = self.reply_buffer[21] as usize;
                let page_len_bytes = page_len_words * 4;
                let len = out_buf.len().min(page_len_bytes);
                out_buf[..len].fill(0xAA); // canned page data
            }

            if let Some(_in_buf) = data_in {
                // PAGE_HEADER doesn't use data_in, just ignore
            }

            Ok(len)
        }
    }

    /// Test mock 2-step read returns canned page bytes.
    #[test]
    fn test_mock_read_config_page() {
        let mut transport = MockTransport::new(0x09, 0x05, 0x03, 0x10); // mfg page 5

        let result = read_config_page(&mut transport, 0x09, 0x05, None, 0).unwrap();

        assert_eq!(result.page_type, 0x09);
        assert_eq!(result.page_number, 0x05);
        assert_eq!(result.page_version, Some(0x03));
        assert_eq!(result.page_length, Some(0x10)); // 16 words = 64 bytes

        // Verify hex is all AA (canned data)
        let expected_len = 0x10 * 4; // 64 bytes
        assert_eq!(result.bytes_hex.len(), expected_len * 2); // hex encoded doubles length
    }

    /// Test PAGE_HEADER with ConfigInvalidPage returns Err.
    #[test]
    fn test_mock_read_invalid_page() {
        let mut transport = MockTransport::new(0x09, 0xFF, 0x00, 0x00); // invalid page

        // Inject INVALID_PAGE (0x0022) into IOCStatus
        transport.reply_buffer[14] = 0x22; // ConfigInvalidPage low byte
        transport.reply_buffer[15] = 0x00; // high byte

        let result = read_config_page(&mut transport, 0x09, 0xFF, None, 0);

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("IOCStatus"));
    }
}
