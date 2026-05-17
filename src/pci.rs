//! PCI device discovery and BAR mapping module for lsi-flash.
//! Port of lsirec.c:194-390 (sysfs walk, BAR1 mmap, driver unbind/rescan).
//! Cites: references/upstream/lsirec-marcan/lsirec.c:194-223 (lsi_open), :314-340 (lsi_unbind_driver), :342-390 (lsi_rescan)
//! Cites: references/oems/card-database.md (card identification data)

use std::fs;
use std::path::PathBuf;

/// PCI device discovered via sysfs walk. Cites lsirec.c:194-223 pattern for reading vendor/device/class.
#[derive(Debug, Clone)]
pub struct PciDevice {
    pub bdf: String,          // e.g., "0000:01:00.0"
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_device_id: u16,
    pub class_code: u32,      // from class file (lsirec.c:205 pattern)
}

impl PciDevice {
    /// Read device attributes from sysfs. Cites lsirec.c:194-223 pattern.
    fn from_sysfs(bdf: &str) -> Result<Self, super::error::PciError> {
        let base = PathBuf::from(format!("/sys/bus/pci/devices/{bdf}"));

        // Read vendor ID (lsirec.c:205 pattern)
        let vendor_str = fs::read_to_string(base.join("vendor"))?;
        let vendor_id = u16::from_str_radix(
            vendor_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read device ID (lsirec.c:205 pattern)
        let device_str = fs::read_to_string(base.join("device"))?;
        let device_id = u16::from_str_radix(
            device_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read subsystem VID/DID (card-database lookup keys)
        let ssid_vid_str = fs::read_to_string(base.join("subsystem_vendor"))?;
        let ssid_vid = u16::from_str_radix(
            ssid_vid_str.trim().trim_start_matches("0x"),
            16,
        )?;

        let ssid_did_str = fs::read_to_string(base.join("subsystem_device"))?;
        let ssid_did = u16::from_str_radix(
            ssid_did_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read class code (lsirec.c:205 pattern)
        let class_str = fs::read_to_string(base.join("class"))?;
        let class_code = u32::from_str_radix(class_str.trim(), 16)?;

        Ok(Self {
            bdf: bdf.to_string(),
            vendor_id,
            device_id,
            subsystem_vendor_id: ssid_vid,
            subsystem_device_id: ssid_did,
            class_code,
        })
    }
}

/// Card identification result. Cites references/oems/card-database.md.
#[derive(Debug, Clone)]
pub struct CardInfo {
    pub name: String,         // e.g., "Dell PERC H200" or "Unknown SAS2008 card"
    pub chip_family: ChipFamily,
    pub flash_size: Option<usize>,
    pub quirks: Vec<Quirk>,   // e.g., TamperCheckRequired
}

/// SAS2008 chip family constant (lsirec.c VID/DID checks).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChipFamily {
    Sas2008,
    Unknown,
}

/// Card-specific quirks that affect flashing procedure.
#[derive(Debug, Clone)]
pub enum Quirk {
    TamperCheckRequired,      // Dell/IBM require megarec or hostboot path
    FujitsuSbrVariantA11,     // D2607 A11 variant (standard SBR)
    FujitsuSbrVariantA21,     // D2607 A21 variant (preserves wiring byte at 0x2A)
}

/// Lookup card info from VID/DID/SSID. Cites references/oems/card-database.md tables.
pub fn identify_card(vid: u16, did: u16, ssid_vid: u16, ssid_did: u16) -> CardInfo {
    // LSI 9211-8i (canonical reference card): VID=0x1000, DID=0x0072, SSID=0x1000:0x3020
    // Cites: sbr_sas9211-8i_itir.cfg:4-9, card-database.md:72
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1000 && ssid_did == 0x3020 {
        return CardInfo {
            name: "LSI 9211-8i".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: Some(722708), // Cites: lsi_sas_hba_crossflash_guide/README.md listing (line 27)
            quirks: vec![],
        };
    }

    // Dell PERC H200e (full-height): VID=0x1000, DID=0x0072, SSID=0x1028:0x1f1c
    // Cites: sbr_dell_h200e_itir.cfg:4-9, card-database.md:81
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1028 && ssid_did == 0x1f1c {
        return CardInfo {
            name: "Dell PERC H200e".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN: no local evidence for exact size
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    // Fujitsu D2607: VID=0x1000, DID=0x0072, SSID=0x1734:0x1177
    // Cites: sbr_fujitsu_d2607_itir.cfg:4-9, card-database.md:115
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1734 && ssid_did == 0x1177 {
        return CardInfo {
            name: "Fujitsu D2607".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN: no local evidence for exact size
            quirks: vec![Quirk::TamperCheckRequired, Quirk::FujitsuSbrVariantA11],
        };
    }

    // IBM M1015: SSID UNKNOWN per card-database.md:99 — mark OPEN
    if vid == 0x1000 && did == 0x0072 {
        return CardInfo {
            name: "Unknown SAS2008 card (VID:DID=1000:0072)".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None, // OPEN
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    CardInfo {
        name: format!("Unknown card (VID:0x{:04x}, DID:0x{:04x})", vid, did),
        chip_family: ChipFamily::Unknown,
        flash_size: None, // OPEN
        quirks: vec![],
    }
}

/// Handle to a mapped PCI device. Cites lsirec.c:194-223 for mmap pattern.
#[derive(Debug)]
pub struct PciHandle {
    pub device: PciDevice,
    pub bar1_mapping: Box<[u8; 4096]>, // or use nix::sys::mmap for safety
}

impl PciHandle {
    /// Open and map BAR1 for a PCI device. Cites lsirec.c:194-223.
    pub fn open(bdf: &str) -> Result<Self, super::error::PciError> {
        let device = PciDevice::from_sysfs(bdf)?;

        // Check if device exists (lsirec.c:205 pattern)
        let resource1_path = format!("/sys/bus/pci/devices/{bdf}/resource1");
        if !std::path::Path::new(&resource1_path).exists() {
            return Err(super::error::PciError::DeviceNotFound { bdf: bdf.to_string() });
        }

        // Open BAR1 (lsirec.c:207-213 pattern)
        let fd = std::fs::File::open(&resource1_path)?;

        // mmap 4KB region with PROT_READ | PROT_WRITE, MAP_SHARED
        // Cites: lsirec.c:213 `mmap(NULL, 0x1000, PROT_READ|PROT_WRITE, MAP_SHARED, fd, 0)`
        let mapping = unsafe {
            let ptr = libc::mmap(
                std::ptr::null_mut(),
                4096,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            );
            if ptr == libc::MAP_FAILED {
                return Err(super::error::PciError::Mmap(format!(
                    "mmap failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            // Convert to typed slice (4KB BAR1)
            let slice = std::slice::from_raw_parts_mut(ptr as *mut u8, 4096);
            let mut array: [u8; 4096] = [0; 4096];
            array.copy_from_slice(slice);
            Box::new(array)
        };

        Ok(Self { device, bar1_mapping: mapping })
    }

    /// Unbind kernel driver from this device. Cites lsirec.c:314-340.
    pub fn unbind_driver(&self) -> Result<(), super::error::PciError> {
        let unbind_path = format!("/sys/bus/pci/devices/{}/driver/unbind", self.device.bdf);

        match std::fs::write(&unbind_path, &self.device.bdf) {
            Ok(_) => {
                println!("Kernel driver unbound from device");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // ENOENT = already unbound; treat as success (lsirec.c:324 pattern)
                println!("Device already unbound or no driver attached");
                Ok(())
            }
            Err(e) => Err(super::error::PciError::Io(e)),
        }
    }

    /// Remove device and rescan PCI bus. Cites lsirec.c:342-390.
    pub fn rescan(&self) -> Result<(), super::error::PciError> {
        println!("Removing PCI device...");

        // Write "1" to /sys/bus/pci/devices/<bdf>/remove (lsirec.c:350-360 pattern)
        let remove_path = format!("/sys/bus/pci/devices/{}/remove", self.device.bdf);
        std::fs::write(&remove_path, "1")?;

        println!("Rescanning PCI bus...");

        // Write "1" to /sys/bus/pci/rescan (lsirec.c:362-370 pattern)
        let rescan_path = "/sys/bus/pci/rescan";
        std::fs::write(rescan_path, "1")?;

        println!("PCI bus rescan complete.");
        Ok(())
    }
}

/// Walk sysfs and return all SAS2008-based devices. Cites lsirec.c:314-390 pattern.
pub fn discover_sas2008_devices() -> Result<Vec<PciDevice>, super::error::PciError> {
    let sysfs_path = PathBuf::from("/sys/bus/pci/devices");
    let mut devices = Vec::new();

    for entry in fs::read_dir(&sysfs_path)? {
        let entry = entry?;
        let bdf = entry.file_name().to_string_lossy().to_string();

        // Skip non-device entries (e.g., "0000:00", "0000:01" are root bridges)
        if !bdf.contains(':') {
            continue;
        }

        match PciDevice::from_sysfs(&bdf) {
            Ok(device) => {
                // Filter for SAS2008 (VID 0x1000, DID 0x0072) or known SSIDs from card-database.md
                if device.vendor_id == 0x1000 && device.device_id == 0x0072 {
                    devices.push(device);
                } else {
                    // Check against card database SSID entries (card-database.md tables)
                    let card_info = identify_card(
                        device.vendor_id,
                        device.device_id,
                        device.subsystem_vendor_id,
                        device.subsystem_device_id,
                    );
                    if matches!(card_info.chip_family, ChipFamily::Sas2008) {
                        devices.push(device);
                    }
                }
            }
            Err(_) => {
                // Device may have been removed; skip silently
                continue;
            }
        }
    }

    Ok(devices)
}
