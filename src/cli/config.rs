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

    /// Which copy to read: current (live), default (firmware built-in), or
    /// nvram (persisted). The default↔current delta = the OEM's customization.
    #[arg(long, value_name = "current|default|nvram", default_value = "current")]
    pub action: String,

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

/// CLI args for `config write` subcommand.
#[derive(Args, Debug)]
pub struct ConfigWriteArgs {
    /// Page type name or hex value (0xNN).
    #[arg(long, value_name = "NAME|0xNN")]
    pub page_type: String,

    /// Page number within the page type.
    #[arg(long)]
    pub page_number: u8,

    /// Extended page type for extended pages (type 0x0F).
    #[arg(long, value_name = "0xNN")]
    pub ext_page_type: Option<String>,

    /// PageAddress field — defaults to 0.
    #[arg(long, value_name = "0xNNNNNNNN", default_value = "0")]
    pub page_address: String,

    /// Full page bytes as hex (must equal PageLength*4 bytes, incl. 4-byte header).
    #[arg(long, value_name = "HEX")]
    pub from_hex: String,

    /// Persist to NVRAM (WRITE_NVRAM). Without this, writes are volatile
    /// (WRITE_CURRENT) and revert on IOC reset. Requires --yes.
    #[arg(long)]
    pub nvram: bool,

    /// Confirm a persistent (--nvram) write. Required for NVRAM writes.
    #[arg(long)]
    pub yes: bool,
}

/// CLI args for `config selftest` subcommand — zero-risk write-path proof.
#[derive(Args, Debug)]
pub struct ConfigSelftestArgs {
    /// Page type name or hex value (0xNN). Default: manufacturing.
    #[arg(long, value_name = "NAME|0xNN", default_value = "manufacturing")]
    pub page_type: String,

    /// Page number. Default: 0 (chip/board identity — persistent, safe to rewrite).
    #[arg(long, default_value = "0")]
    pub page_number: u8,
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

/// MPI2_CONFIG_ACTION_* constants — mpi2_cnfg.h:353-360.
pub mod action {
    pub const PAGE_HEADER: u8 = 0x00;
    pub const READ_CURRENT: u8 = 0x01;
    pub const WRITE_CURRENT: u8 = 0x02; // volatile — reverts on IOC reset
    pub const READ_DEFAULT: u8 = 0x05; // firmware built-in defaults
    pub const READ_NVRAM: u8 = 0x06; // persisted NVRAM values
    pub const WRITE_NVRAM: u8 = 0x04; // persists to NVRAM
}

/// Read a single config page via Mpt3CtlTransport.
///
/// Implements the 2-step pattern from mpt3sas_config.c:_config_request:
/// 1. PAGE_HEADER (action=0x00) → get PageLength from reply header
/// 2. `data_action` (READ_CURRENT 0x01 / READ_DEFAULT 0x05 / READ_NVRAM 0x06)
///    with a buffer of PageLength*4 bytes.
pub fn read_config_page(
    transport: &mut dyn crate::mpt::MptTransport,
    page_type: u8,
    page_number: u8,
    ext_page_type: Option<u8>,
    page_address: u32,
) -> Result<ConfigPageRead, String> {
    read_config_page_action(
        transport,
        page_type,
        page_number,
        ext_page_type,
        page_address,
        action::READ_CURRENT,
    )
}

/// Read a config page with an explicit data-read action (CURRENT/DEFAULT/NVRAM).
pub fn read_config_page_action(
    transport: &mut dyn crate::mpt::MptTransport,
    page_type: u8,
    page_number: u8,
    ext_page_type: Option<u8>,
    page_address: u32,
    data_action: u8,
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
        .send_with_sge_offset(&req_bytes, 7, &mut reply_buf, None, None)
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

    // The reply PageType carries attribute bits in the upper nibble
    // (MPI2_CONFIG_PAGEATTR_*: READ_ONLY 0x00 / CHANGEABLE 0x10 / PERSISTENT 0x20),
    // so mask with MPI2_CONFIG_PAGETYPE_MASK (0x0F) before comparing.
    const PAGETYPE_MASK: u8 = 0x0F; // mpi2_cnfg.h
    const PAGETYPE_EXTENDED: u8 = 0x0F; // MPI2_CONFIG_PAGETYPE_EXTENDED

    // PageNumber always echoes.
    if page_hdr.number != page_number {
        return Err(format!(
            "Page number mismatch: requested {}/{} got {:#04x}/{}",
            page_type, page_number, page_hdr.type_, page_hdr.number
        ));
    }

    // Length source differs by page kind. Standard pages: 1-byte PageLength at
    // reply 0x15 (page_hdr.length). EXTENDED pages (type 0x0F, e.g. FLASH_LAYOUT
    // 0x14): U16 ExtPageLength at reply 0x04, and ExtPageType at reply 0x06.
    let is_ext = (page_type & PAGETYPE_MASK) == PAGETYPE_EXTENDED;
    let page_len_words = if is_ext {
        if (page_hdr.type_ & PAGETYPE_MASK) != PAGETYPE_EXTENDED {
            return Err(format!(
                "expected EXTENDED page (0x0F), reply PageType={:#04x}",
                page_hdr.type_
            ));
        }
        let reply_ext_type = reply_buf[0x06];
        if let Some(want) = ext_page_type {
            if reply_ext_type != want {
                return Err(format!(
                    "ExtPageType mismatch: requested {:#04x} got {:#04x}",
                    want, reply_ext_type
                ));
            }
        }
        u16::from_le_bytes([reply_buf[0x04], reply_buf[0x05]]) as usize
    } else {
        if (page_hdr.type_ & PAGETYPE_MASK) != (page_type & PAGETYPE_MASK) {
            return Err(format!(
                "Page type mismatch: requested {:#04x} got {:#04x}",
                page_type, page_hdr.type_
            ));
        }
        page_hdr.length as usize
    };
    if page_len_words == 0 {
        return Err("page length = 0 from firmware".to_string());
    }
    let page_buf_size = page_len_words * 4;

    // Step 2: data read (CURRENT/DEFAULT/NVRAM) — fetch full page data.
    let mut page_buf = vec![0u8; page_buf_size];
    let req_read = ConfigRequest {
        action: data_action, // READ_CURRENT 0x01 / READ_DEFAULT 0x05 / READ_NVRAM 0x06
        sgl_flags: 0xC0,     // END_OF_LIST + IOC_TO_HOST
        page_type,
        page_number,
        ext_page_type,
        payload_buffer: &mut page_buf,
        page_address,
    };

    let req_bytes = req_read.serialize_to(2);
    transport
        .send_with_sge_offset(&req_bytes, 7, &mut reply_buf, Some(&mut page_buf), None)
        .map_err(|e| format!("send data-read (action {:#04x}) failed: {}", data_action, e))?;

    let reply_data =
        ConfigReply::parse(&reply_buf).map_err(|e| format!("CONFIG read parse failed: {}", e))?;

    if reply_data.ioc_status != IocStatus::Success {
        return Err(format!(
            "data-read (action {:#04x}) failed with IOCStatus={:?}",
            data_action, reply_data.ioc_status
        ));
    }

    Ok(ConfigPageRead {
        page_type,
        page_number,
        page_version: Some(page_hdr.version),
        // Real length in dwords (ext pages use ExtPageLength, not the 0x15 byte).
        page_length: Some(page_len_words.min(255) as u8),
        ext_page_type,
        bytes_hex: hex::encode(&page_buf),
    })
}

/// Write a full config page via Mpt3CtlTransport.
///
/// 2-step like the read: PAGE_HEADER (validate + size), then WRITE_CURRENT
/// (0x02, volatile — reverts on IOC reset) or WRITE_NVRAM (0x04, persists),
/// with the page bytes flowing host→IOC via `data_out` (the reverse of a read).
/// `data` MUST be exactly PageLength*4 bytes — the full page including its
/// 4-byte header (write back what you read, then mutate).
///
/// Safety: WRITE_NVRAM is persistent. Callers gate it behind explicit
/// confirmation. WRITE_CURRENT is reversible by an IOC reset / power cycle.
pub fn write_config_page(
    transport: &mut dyn crate::mpt::MptTransport,
    page_type: u8,
    page_number: u8,
    ext_page_type: Option<u8>,
    page_address: u32,
    data: &[u8],
    persist: bool,
) -> Result<(), String> {
    use crate::mpi::messages::{ConfigReply, ConfigRequest, IocStatus};

    // Step 1: PAGE_HEADER — get expected PageLength + validate type/number.
    let mut header_buf = [0u8; 4];
    let req_header = ConfigRequest {
        action: action::PAGE_HEADER,
        sgl_flags: 0xC0,
        page_type,
        page_number,
        ext_page_type,
        payload_buffer: &mut header_buf,
        page_address,
    };
    let req_bytes = req_header.serialize_to(1);
    let mut reply_buf = [0u8; 256];
    transport
        .send_with_sge_offset(&req_bytes, 7, &mut reply_buf, None, None)
        .map_err(|e| format!("send PAGE_HEADER failed: {}", e))?;
    let reply_header =
        ConfigReply::parse(&reply_buf).map_err(|e| format!("CONFIG header parse failed: {}", e))?;
    if reply_header.ioc_status != IocStatus::Success {
        return Err(format!(
            "PAGE_HEADER failed with IOCStatus={:?} (page may not exist)",
            reply_header.ioc_status
        ));
    }
    let page_hdr = PageHeader::parse(&reply_header.page_header);
    let expected = page_hdr.length as usize * 4;
    if expected == 0 {
        return Err("PageLength=0 from firmware".to_string());
    }
    if data.len() != expected {
        return Err(format!(
            "write data length {} != page size {} (PageLength={} words)",
            data.len(),
            expected,
            page_hdr.length
        ));
    }

    // Step 2: WRITE_CURRENT / WRITE_NVRAM — page bytes flow host→IOC (data_out).
    let mut page_buf = data.to_vec();
    let write_action = if persist {
        action::WRITE_NVRAM
    } else {
        action::WRITE_CURRENT
    };
    let req_write = ConfigRequest {
        action: write_action,
        sgl_flags: 0x80, // END_OF_LIST, host→IOC (kernel builds the real SGE from data_out)
        page_type,
        page_number,
        ext_page_type,
        payload_buffer: &mut page_buf,
        page_address,
    };
    let req_bytes = req_write.serialize_to(2);
    transport
        .send_with_sge_offset(&req_bytes, 7, &mut reply_buf, None, Some(&mut page_buf))
        .map_err(|e| format!("send write (action {:#04x}) failed: {}", write_action, e))?;
    let reply_data =
        ConfigReply::parse(&reply_buf).map_err(|e| format!("CONFIG write parse failed: {}", e))?;
    if reply_data.ioc_status != IocStatus::Success {
        return Err(format!(
            "write (action {:#04x}) failed with IOCStatus={:?}",
            write_action, reply_data.ioc_status
        ));
    }
    Ok(())
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

/// Map an `--action` string to a data-read action constant.
fn read_action_from_str(s: &str) -> Result<u8, String> {
    match s.to_lowercase().as_str() {
        "current" => Ok(action::READ_CURRENT),
        "default" => Ok(action::READ_DEFAULT),
        "nvram" => Ok(action::READ_NVRAM),
        _ => Err(format!(
            "Unknown read action '{}'. Use: current, default, nvram",
            s
        )),
    }
}

fn parse_page_address(s: &str) -> Result<u32, crate::Error> {
    u32::from_str_radix(s.strip_prefix("0x").unwrap_or(s), 16)
        .map_err(|e| crate::Error::Other(format!("Invalid page_address: {}", e)))
}

/// Run `config read` subcommand.
#[allow(clippy::too_many_arguments)]
pub fn run_read(
    bdf: String,
    page_type: String,
    page_number: u8,
    ext_page_type: Option<String>,
    page_address: String,
    action_str: String,
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

    let page_address_val = parse_page_address(&page_address)?;
    let data_action = read_action_from_str(&action_str).map_err(crate::Error::Other)?;

    let result = read_config_page_action(
        &mut transport,
        page_type_val,
        page_number,
        ext_page_type_val,
        page_address_val,
        data_action,
    )
    .map_err(crate::Error::Other)?;

    if json_flag {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("{}", format_hexdump(&result));
    }

    Ok(())
}

/// Run `config write` subcommand.
pub fn run_write(bdf: String, args: ConfigWriteArgs) -> Result<(), crate::Error> {
    use crate::mpt::Mpt3CtlTransport;

    let page_type_val = page_type_from_str(&args.page_type).map_err(crate::Error::Other)?;
    let ext_page_type_val = if let Some(et) = &args.ext_page_type {
        Some(ext_page_type_from_str(et).map_err(crate::Error::Other)?)
    } else {
        None
    };
    let page_address_val = parse_page_address(&args.page_address)?;
    let data = hex::decode(args.from_hex.trim())
        .map_err(|e| crate::Error::Other(format!("Invalid --from-hex: {}", e)))?;

    if args.nvram && !args.yes {
        return Err(crate::Error::Other(
            "Refusing persistent NVRAM write without --yes. WRITE_NVRAM is permanent; \
             re-run with --yes to confirm (or omit --nvram for a volatile WRITE_CURRENT)."
                .to_string(),
        ));
    }

    let mut transport = Mpt3CtlTransport::open(&bdf)
        .map_err(|e| crate::Error::Other(format!("mpt3ctl open: {}", e)))?;

    write_config_page(
        &mut transport,
        page_type_val,
        args.page_number,
        ext_page_type_val,
        page_address_val,
        &data,
        args.nvram,
    )
    .map_err(crate::Error::Other)?;

    let mode = if args.nvram {
        "WRITE_NVRAM (persisted)"
    } else {
        "WRITE_CURRENT (volatile — reverts on IOC reset)"
    };
    println!(
        "OK: wrote {} bytes to {}#{} via {}",
        data.len(),
        page_type_name(page_type_val),
        args.page_number,
        mode
    );
    Ok(())
}

/// Run `config selftest` — zero-risk proof of the write path.
///
/// Reads a page, writes the IDENTICAL bytes back via WRITE_CURRENT (volatile),
/// re-reads, and asserts the page is unchanged. This validates the write wire
/// format (data direction, SGE, action) WITHOUT altering any persisted state —
/// WRITE_CURRENT is volatile and we write back exactly what we read.
pub fn run_selftest(bdf: String, args: ConfigSelftestArgs) -> Result<(), crate::Error> {
    use crate::mpt::Mpt3CtlTransport;

    let page_type_val = page_type_from_str(&args.page_type).map_err(crate::Error::Other)?;
    let pn = args.page_number;

    let mut transport = Mpt3CtlTransport::open(&bdf)
        .map_err(|e| crate::Error::Other(format!("mpt3ctl open: {}", e)))?;

    println!(
        "config selftest: idempotent WRITE_CURRENT round-trip on {}#{} (no state change)",
        page_type_name(page_type_val),
        pn
    );

    // 1. Read current.
    let before = read_config_page(&mut transport, page_type_val, pn, None, 0)
        .map_err(|e| crate::Error::Other(format!("read (before) failed: {}", e)))?;
    println!("  read before:  {} bytes", before.bytes_hex.len() / 2);

    // 2. Write identical bytes back via WRITE_CURRENT (volatile, idempotent).
    let bytes = hex::decode(&before.bytes_hex)
        .map_err(|e| crate::Error::Other(format!("decode: {}", e)))?;
    write_config_page(&mut transport, page_type_val, pn, None, 0, &bytes, false)
        .map_err(|e| crate::Error::Other(format!("WRITE_CURRENT failed: {}", e)))?;
    println!("  WRITE_CURRENT: Success (idempotent — wrote back identical bytes)");

    // 3. Re-read and compare.
    let after = read_config_page(&mut transport, page_type_val, pn, None, 0)
        .map_err(|e| crate::Error::Other(format!("read (after) failed: {}", e)))?;

    if before.bytes_hex == after.bytes_hex {
        println!("  read after:   identical ✓");
        println!("PASS: write path validated, zero state change.");
        Ok(())
    } else {
        Err(crate::Error::Other(format!(
            "FAIL: page changed across idempotent write!\n  before: {}\n  after:  {}",
            before.bytes_hex, after.bytes_hex
        )))
    }
}

/// Run `config dump` subcommand — enumerate every config page that exists.
///
/// Probes the known (type, number) space; pages that don't exist return a
/// non-Success IOCStatus from `read_config_page` (INVALID_PAGE/INVALID_TYPE)
/// and are skipped silently. Surfaces Manufacturing Page 43 (ISTWI device
/// table) and everything else the card exposes.
///
/// NOTE: extended pages (type 0x0F, e.g. FLASH_LAYOUT 0x14) use an 8-byte
/// ExtPageHeader; `read_config_page`/`PageHeader::parse` currently assume the
/// 4-byte standard header, so extended results here are best-effort and may be
/// length-truncated. Standard pages (Manufacturing/IO-Unit/IOC/BIOS) are exact.
pub fn run_dump(bdf: String, json_flag: bool) -> Result<(), crate::Error> {
    use crate::mpt::Mpt3CtlTransport;

    let mut transport = Mpt3CtlTransport::open(&bdf)
        .map_err(|e| crate::Error::Other(format!("mpt3ctl open: {}", e)))?;

    // Standard (type, max_number) probe space — generous upper bounds.
    // Cites mpi2_cnfg.h:207-213 for the page-type values.
    let standard: &[(u8, u8)] = &[
        (0x00, 15), // IO Unit
        (0x01, 15), // IOC
        (0x02, 7),  // BIOS
        (0x08, 3),  // RAID Volume
        (0x09, 63), // Manufacturing (incl. Man Page 43 = ISTWI device table)
        (0x0A, 2),  // RAID PhysDisk
    ];
    // Extended page types (under type 0x0F) — best-effort (see NOTE above).
    let ext_types: &[u8] = &[0x10, 0x12, 0x13, 0x14];

    let mut entries: Vec<ConfigDumpEntry> = Vec::new();

    for &(pt, max_num) in standard {
        for num in 0..=max_num {
            if let Ok(page) = read_config_page(&mut transport, pt, num, None, 0) {
                entries.push(ConfigDumpEntry {
                    page_type: pt,
                    page_number: num,
                    length: page.page_length.unwrap_or(0),
                    ext_page_type: None,
                    bytes_hex: Some(page.bytes_hex),
                });
            }
        }
    }

    for &et in ext_types {
        for num in 0..=3u8 {
            if let Ok(page) = read_config_page(&mut transport, 0x0F, num, Some(et), 0) {
                entries.push(ConfigDumpEntry {
                    page_type: 0x0F,
                    page_number: num,
                    length: page.page_length.unwrap_or(0),
                    ext_page_type: Some(et),
                    bytes_hex: Some(page.bytes_hex),
                });
            }
        }
    }

    if json_flag {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        println!("Config page inventory — {} pages found:", entries.len());
        println!("  {:<16} {:>6} {:>8}  EXT", "TYPE", "NUMBER", "LEN(w)");
        for e in &entries {
            let ext = e
                .ext_page_type
                .map(|x| format!("0x{:02X}", x))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {:<16} {:>6} {:>8}  {}",
                page_type_name(e.page_type),
                e.page_number,
                e.length,
                ext
            );
        }
        println!();
        for e in &entries {
            let ext_str = e
                .ext_page_type
                .map(|x| format!(" ext=0x{:02X}", x))
                .unwrap_or_default();
            println!(
                "== {}#{}{} (len={} words) ==",
                page_type_name(e.page_type),
                e.page_number,
                ext_str,
                e.length
            );
            if let Some(hex_s) = &e.bytes_hex {
                let bytes = hex::decode(hex_s).unwrap_or_default();
                for (i, chunk) in bytes.chunks(16).enumerate() {
                    let hexs: String = chunk.iter().map(|b| format!("{:02x}", b)).collect();
                    println!("  {:04x}: {}", i * 16, hexs);
                }
            }
            println!();
        }
    }

    Ok(())
}

/// Config subcommands.
#[derive(clap::Subcommand, Debug)]
pub enum ConfigSubCommand {
    /// Read a single config page from the chip (--action current|default|nvram).
    #[clap(name = "read")]
    Read(ConfigReadArgs),

    /// Enumerate all existing config pages on the card.
    #[clap(name = "dump")]
    Dump(ConfigDumpArgs),

    /// Write a full config page (volatile WRITE_CURRENT; --nvram --yes to persist).
    #[clap(name = "write")]
    Write(ConfigWriteArgs),

    /// Zero-risk write-path proof: idempotent WRITE_CURRENT round-trip.
    #[clap(name = "selftest")]
    Selftest(ConfigSelftestArgs),
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
            args.action,
            args.json,
        ),
        ConfigSubCommand::Dump(args) => run_dump(bdf, args.json),
        ConfigSubCommand::Write(args) => run_write(bdf, args),
        ConfigSubCommand::Selftest(args) => run_selftest(bdf, args),
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

        // MPI2_CONFIG_REQUEST (no preceding header): Action@0x00, Function@0x03,
        // PageHeader@0x14 (Ver,Len,Num,Type at 0x14..0x17), PageAddress@0x18.
        assert_eq!(wire[0x00], 0x01, "Action READ_CURRENT at offset 0x00");
        assert_eq!(wire[0x03], 0x04, "Function CONFIG (0x04) at offset 0x03");
        assert_eq!(wire[0x16], 0x00, "PageNumber at offset 0x16");
        assert_eq!(
            wire[0x17], 0x09,
            "PageType manufacturing (0x09) at offset 0x17"
        );

        let page_addr = u32::from_le_bytes([wire[0x18], wire[0x19], wire[0x1A], wire[0x1B]]);
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

        // PageAddress at offset 0x18 (mpi2_cnfg.h:347).
        let page_addr = u32::from_le_bytes([wire[0x18], wire[0x19], wire[0x1A], wire[0x1B]]);

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

            // READ_CURRENT page data flows IOC→host via data_in (mirrors FW_UPLOAD).
            if let Some(in_buf) = data_in {
                let page_len_bytes = self.reply_buffer[21] as usize * 4;
                let len = in_buf.len().min(page_len_bytes);
                in_buf[..len].fill(0xAA); // canned page data
            }

            // CONFIG reads never use data_out (that is host→IOC, e.g. FW_DOWNLOAD).
            debug_assert!(data_out.is_none(), "CONFIG read must not use data_out");

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

    /// Mock that records the actions it saw and the bytes sent host→IOC.
    struct WriteMock {
        reply_buffer: Vec<u8>,
        actions: Vec<u8>,
        last_data_out: Vec<u8>,
    }

    impl WriteMock {
        fn new(page_type: u8, page_number: u8, length: u8) -> Self {
            let mut buf = vec![0u8; 256];
            buf[20] = 0x00; // version
            buf[21] = length; // PageLength (words)
            buf[22] = page_number;
            buf[23] = page_type;
            Self {
                reply_buffer: buf,
                actions: Vec::new(),
                last_data_out: Vec::new(),
            }
        }
    }

    impl crate::mpt::MptTransport for WriteMock {
        fn send_with_sge_offset(
            &mut self,
            request: &[u8],
            _off: u32,
            reply: &mut [u8],
            _data_in: Option<&mut [u8]>,
            data_out: Option<&mut [u8]>,
        ) -> Result<usize, crate::mpt::TransportError> {
            self.actions.push(request[0]); // Action @ 0x00
            if let Some(out) = data_out {
                self.last_data_out = out.to_vec();
            }
            let len = reply.len().min(self.reply_buffer.len());
            reply[..len].copy_from_slice(&self.reply_buffer[..len]);
            Ok(len)
        }
    }

    /// WRITE_CURRENT: PAGE_HEADER then action 0x02, data sent via data_out.
    #[test]
    fn test_write_config_page_current() {
        let mut t = WriteMock::new(0x09, 0x00, 2); // 2 words = 8 bytes
        let data = vec![0xDEu8; 8];
        write_config_page(&mut t, 0x09, 0x00, None, 0, &data, false).unwrap();
        assert_eq!(t.actions, vec![action::PAGE_HEADER, action::WRITE_CURRENT]);
        assert_eq!(
            t.last_data_out, data,
            "page bytes must flow host->IOC (data_out)"
        );
    }

    /// WRITE_NVRAM uses action 0x04.
    #[test]
    fn test_write_config_page_nvram_action() {
        let mut t = WriteMock::new(0x09, 0x00, 2);
        write_config_page(&mut t, 0x09, 0x00, None, 0, &[0u8; 8], true).unwrap();
        assert_eq!(t.actions, vec![action::PAGE_HEADER, action::WRITE_NVRAM]);
    }

    /// Wrong data length is rejected before any write is sent.
    #[test]
    fn test_write_config_page_length_mismatch() {
        let mut t = WriteMock::new(0x09, 0x00, 2); // expects 8 bytes
        let err = write_config_page(&mut t, 0x09, 0x00, None, 0, &[0u8; 4], false).unwrap_err();
        assert!(err.contains("length"));
        assert_eq!(t.actions, vec![action::PAGE_HEADER]); // never reached the write
    }
}
