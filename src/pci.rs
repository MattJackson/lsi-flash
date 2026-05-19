//! PCI device discovery and BAR mapping module for lsi-flash.
//! Port of lsirec.c:194-390 (sysfs walk, BAR1 mmap, driver unbind/rescan).
//! Cites: references/upstream/lsirec-marcan/lsirec.c:194-223 (lsi_open), :314-340 (lsi_unbind_driver), :342-390 (lsi_rescan)
//! Cites: references/oems/card-database.md (card identification data)

#[cfg(test)] use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::ptr;

/// Abstraction over OS-specific operations the PCI module needs.
/// Allows unit tests to inject a mock filesystem without real sysfs.
pub trait Platform {
    /// Read a UTF-8 file (e.g., `/sys/bus/pci/devices/.../vendor`).
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    /// Write to a file (e.g., `/sys/bus/pci/drivers/.../unbind`).
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;

    /// Read directory entries (e.g., `/sys/bus/pci/devices/`).
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// mmap a file at the given offset/length. For BAR1 mmap. Returns owned bytes.
    fn mmap_ro(&self, path: &Path, offset: u64, len: usize) -> io::Result<Box<[u8]>>;
}

/// Production platform: real Linux sysfs + mmap.
pub struct LinuxSysfs;

impl Platform for LinuxSysfs {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            entries.push(entry?.path());
        }
        Ok(entries)
    }

    fn mmap_ro(&self, path: &Path, offset: u64, len: usize) -> io::Result<Box<[u8]>> {
        let fd = std::fs::File::open(path)?;
        unsafe {
            let ptr = libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                offset as i64,
            );
            if ptr == libc::MAP_FAILED {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("mmap failed: {}", io::Error::last_os_error()),
                ));
            }
            let slice = std::slice::from_raw_parts(ptr as *const u8, len);
            let vec: Vec<u8> = slice.to_vec();
            libc::munmap(ptr, len);
            Ok(vec.into_boxed_slice())
        }
    }
}

/// Mock platform for unit tests. Allows injecting fake sysfs data.
#[cfg(test)]
pub struct MockPlatform {
    files: HashMap<PathBuf, Vec<u8>>,
    dirs: HashMap<PathBuf, Vec<PathBuf>>,
}

#[cfg(test)]
impl MockPlatform {
    pub fn new() -> Self {
        let mut mock = Self {
            files: HashMap::new(),
            dirs: HashMap::new(),
        };
        // Initialize the devices directory for empty-dir tests
        mock.dirs.insert(
            PathBuf::from("/sys/bus/pci/devices"),
            Vec::new(),
        );
        mock
    }

    pub fn add_file(&mut self, path: &str, contents: &str) {
        let pb = PathBuf::from(path);
        self.files.insert(pb, contents.as_bytes().to_vec());
    }

    pub fn add_device(
        &mut self,
        bdf: &str,
        vendor: u16,
        device: u16,
        ssvid: u16,
        ssdid: u16,
        class: u32,
    ) {
        let base = format!("/sys/bus/pci/devices/{bdf}");
        self.add_file(&format!("{base}/vendor"), &format!("0x{:04x}", vendor));
        self.add_file(&format!("{base}/device"), &format!("0x{:04x}", device));
        self.add_file(
            &format!("{base}/subsystem_vendor"),
            &format!("0x{:04x}", ssvid),
        );
        self.add_file(&format!("{base}/subsystem_device"), &format!("0x{:04x}", ssdid));
        self.add_file(&format!("{base}/class"), &format!("{:06x}", class));

        let bdf_path = PathBuf::from(bdf);
        self.dirs
            .entry(PathBuf::from("/sys/bus/pci/devices"))
            .or_insert_with(Vec::new)
            .push(bdf_path);
    }
}

#[cfg(test)]
impl Default for MockPlatform {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
impl Platform for MockPlatform {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        self.files
            .get(path)
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "file not found"))
            .map(|bytes| String::from_utf8_lossy(bytes).to_string())
    }

    fn write(&self, _path: &Path, _contents: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.dirs
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "dir not found"))
    }

    fn mmap_ro(&self, _path: &Path, _offset: u64, _len: usize) -> io::Result<Box<[u8]>> {
        Ok(vec![0u8; 4096].into_boxed_slice())
    }
}

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
    fn from_sysfs<P: Platform>(bdf: &str, plat: &P) -> Result<Self, super::error::PciError> {
        let base = PathBuf::from(format!("/sys/bus/pci/devices/{bdf}"));

        // Read vendor ID (lsirec.c:205 pattern)
        let vendor_str = plat.read_to_string(&base.join("vendor"))?;
        let vendor_id = u16::from_str_radix(
            vendor_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read device ID (lsirec.c:205 pattern)
        let device_str = plat.read_to_string(&base.join("device"))?;
        let device_id = u16::from_str_radix(
            device_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read subsystem VID/DID (card-database lookup keys)
        let ssid_vid_str = plat.read_to_string(&base.join("subsystem_vendor"))?;
        let ssid_vid = u16::from_str_radix(
            ssid_vid_str.trim().trim_start_matches("0x"),
            16,
        )?;

        let ssid_did_str = plat.read_to_string(&base.join("subsystem_device"))?;
        let ssid_did = u16::from_str_radix(
            ssid_did_str.trim().trim_start_matches("0x"),
            16,
        )?;

        // Read class code (lsirec.c:205 pattern)
        let class_str = plat.read_to_string(&base.join("class"))?;
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
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1000 && ssid_did == 0x3020 {
        return CardInfo {
            name: "LSI 9211-8i".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: Some(722708),
            quirks: vec![],
        };
    }

    // Dell PERC H200e (full-height): VID=0x1000, DID=0x0072, SSID=0x1028:0x1f1c
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1028 && ssid_did == 0x1f1c {
        return CardInfo {
            name: "Dell PERC H200e".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None,
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    // Fujitsu D2607: VID=0x1000, DID=0x0072, SSID=0x1734:0x1177
    if vid == 0x1000 && did == 0x0072 && ssid_vid == 0x1734 && ssid_did == 0x1177 {
        return CardInfo {
            name: "Fujitsu D2607".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None,
            quirks: vec![Quirk::TamperCheckRequired, Quirk::FujitsuSbrVariantA11],
        };
    }

    // IBM M1015: SSID UNKNOWN per card-database.md:99 — mark OPEN
    if vid == 0x1000 && did == 0x0072 {
        return CardInfo {
            name: "Unknown SAS2008 card (VID:DID=1000:0072)".to_string(),
            chip_family: ChipFamily::Sas2008,
            flash_size: None,
            quirks: vec![Quirk::TamperCheckRequired],
        };
    }

    CardInfo {
        name: format!("Unknown card (VID:0x{:04x}, DID:0x{:04x})", vid, did),
        chip_family: ChipFamily::Unknown,
        flash_size: None,
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
    pub fn open<P: Platform>(bdf: &str, plat: &P) -> Result<Self, super::error::PciError> {
        let device = PciDevice::from_sysfs(bdf, plat)?;

        // Check if device exists (lsirec.c:205 pattern)
        let resource1_path = format!("/sys/bus/pci/devices/{bdf}/resource1");
        if !std::path::Path::new(&resource1_path).exists() {
            return Err(super::error::PciError::DeviceNotFound { bdf: bdf.to_string() });
        }

        // Open BAR1 (lsirec.c:207-213 pattern) - use mmap_ro abstraction
        let mapping_bytes = plat.mmap_ro(
            PathBuf::from(&resource1_path).as_path(),
            0,
            4096,
        )?;

        // Convert Box<[u8]> to Box<[u8; 4096]>
        if mapping_bytes.len() != 4096 {
            return Err(super::error::PciError::Mmap("BAR1 mapping size mismatch".to_string()));
        }
        let mut array: [u8; 4096] = [0; 4096];
        array.copy_from_slice(&mapping_bytes);

        Ok(Self { device, bar1_mapping: Box::new(array) })
    }

    /// Unbind kernel driver from this device. Cites lsirec.c:314-340.
    pub fn unbind_driver<P: Platform>(&self, plat: &P) -> Result<(), super::error::PciError> {
        let unbind_path = format!("/sys/bus/pci/devices/{}/driver/unbind", self.device.bdf);

        match plat.write(&PathBuf::from(&unbind_path).as_path(), self.device.bdf.as_bytes()) {
            Ok(_) => {
                println!("Kernel driver unbound from device");
                Ok(())
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                // ENOENT = already unbound; treat as success (lsirec.c:324 pattern)
                println!("Device already unbound or no driver attached");
                Ok(())
            }
            Err(e) => Err(super::error::PciError::Io(e)),
        }
    }

    /// Remove device and rescan PCI bus. Cites lsirec.c:342-390.
    pub fn rescan<P: Platform>(&self, plat: &P) -> Result<(), super::error::PciError> {
        println!("Removing PCI device...");

        // Write "1" to /sys/bus/pci/devices/<bdf>/remove (lsirec.c:350-360 pattern)
        let remove_path = format!("/sys/bus/pci/devices/{}/remove", self.device.bdf);
        plat.write(&PathBuf::from(remove_path), b"1")?;

        println!("Rescanning PCI bus...");

        // Write "1" to /sys/bus/pci/rescan (lsirec.c:362-370 pattern)
        let rescan_path = "/sys/bus/pci/rescan";
        plat.write(&PathBuf::from(rescan_path), b"1")?;

        println!("PCI bus rescan complete.");
        Ok(())
    }
}

/// Walk sysfs and return all SAS2008-based devices. Cites lsirec.c:314-390 pattern.
pub fn discover_sas2008_devices<P: Platform>(plat: &P) -> Result<Vec<PciDevice>, super::error::PciError> {
    let sysfs_path = PathBuf::from("/sys/bus/pci/devices");
    let mut devices = Vec::new();

    for entry in plat.read_dir(&sysfs_path)? {
        let bdf = match entry.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => continue,
        };

        // Skip non-device entries (e.g., "0000:00", "0000:01" are root bridges)
        if !bdf.contains(':') {
            continue;
        }

        match PciDevice::from_sysfs(&bdf, plat) {
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

/// Convenience wrapper using LinuxSysfs for backward compatibility.
pub fn discover_sas2008_devices_linux() -> Result<Vec<PciDevice>, super::error::PciError> {
    discover_sas2008_devices(&LinuxSysfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_finds_lsi_device_in_mock() {
        let mut mock = MockPlatform::new();
        mock.add_device("0000:01:00.0", 0x1000, 0x0072, 0x1028, 0x1F1D, 0x010700);
        let devices = discover_sas2008_devices(&mock).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].bdf, "0000:01:00.0");
        assert_eq!(devices[0].vendor_id, 0x1000);
        assert_eq!(devices[0].device_id, 0x0072);
        assert_eq!(devices[0].subsystem_device_id, 0x1F1D);
    }

    #[test]
    fn enumerate_skips_non_lsi_devices() {
        let mut mock = MockPlatform::new();
        mock.add_device("0000:00:00.0", 0x8086, 0x1234, 0, 0, 0x060000); // Intel, host bridge
        mock.add_device("0000:01:00.0", 0x1000, 0x0072, 0x1028, 0x1F1D, 0x010700);
        let devices = discover_sas2008_devices(&mock).unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].vendor_id, 0x1000);
    }

    #[test]
    fn enumerate_handles_empty_dir() {
        let mock = MockPlatform::new();
        let devices = discover_sas2008_devices(&mock).unwrap();
        assert!(devices.is_empty());
    }
}
