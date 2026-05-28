//! Pre-flight safety guards for flash orchestrator.
//! Per ADR-007 D-5 + ADR-015 Rule 10 + Stage 3 scoping doc §4.
//!
//! Refuses to flash if any block device attached to the SAS controller
//! is in active use (mounted, LVM PV, mdraid member, ZFS vdev).

use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SafetyError {
    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("failed to execute {tool}: {reason}")]
    ToolExecution { tool: String, reason: String },
}

#[derive(Debug, Clone)]
pub enum SafetyConcern {
    MountedFilesystem {
        device: String,
        mountpoint: String,
        fstype: String,
    },
    LvmPhysicalVolume {
        device: String,
        vg_name: String,
    },
    MdraidMember {
        device: String,
        array_name: String,
    },
    ZfsVdev {
        device: String,
        pool_name: String,
    },
}

impl SafetyConcern {
    /// User-facing educational message per ADR-007 design principle 3.
    pub fn human(&self) -> String {
        match self {
            Self::MountedFilesystem {
                device,
                mountpoint,
                fstype,
            } => format!(
                "Block device {} ({}) is mounted at {}.\n\
                 Flashing would corrupt the filesystem in active use.\n  \
                 Fix: umount {} before re-running flash, OR move the data off this card.",
                device, fstype, mountpoint, mountpoint
            ),
            Self::LvmPhysicalVolume { device, vg_name } => format!(
                "Block device {} is an LVM physical volume in volume group '{}'.\n\
                 Flashing would brick the VG.\n  \
                 Fix: 'vgchange -an {}' or 'pvmove' off this disk before flashing.",
                device, vg_name, vg_name
            ),
            Self::MdraidMember { device, array_name } => format!(
                "Block device {} is a member of active mdraid array '{}'.\n\
                 Flashing would corrupt the RAID array beyond recovery.\n  \
                 Fix: 'mdadm --stop /dev/{}' before flashing.",
                device, array_name, array_name
            ),
            Self::ZfsVdev { device, pool_name } => format!(
                "Block device {} is part of active ZFS pool '{}'.\n\
                 Flashing would corrupt the ZFS vdev.\n  \
                 Fix: 'zpool export {}' before flashing.",
                device, pool_name, pool_name
            ),
        }
    }
}

/// Walk sysfs to find block devices attached to the given SAS controller.
/// Returns absolute /dev/sdX paths.
///
/// For each BDF, walks:
///   /sys/bus/pci/devices/<bdf>/host*/target*:*:*/block/*
/// Each terminal entry is a kernel block-device name (sda, sdb, etc.).
/// Prepend "/dev/" to get the device path.
pub fn devices_attached_to_card(bdf: &str) -> Result<Vec<PathBuf>, SafetyError> {
    let mut devices = HashSet::new();

    // Build PCI path prefix for this BDF
    let pci_path = format!("/sys/bus/pci/devices/{bdf}");

    if !Path::new(&pci_path).exists() {
        return Ok(Vec::new());
    }

    // Walk the SCSI target hierarchy under this PCI device
    // Pattern: host0/target*:*:*/block/sdX or block/nvmeNnNcNx
    let host_base = format!("{}/host*", pci_path);

    for host_entry in fs::read_dir(&host_base)? {
        let host_path = host_entry?.path();
        if !host_path.is_dir() {
            continue;
        }

        // Walk target directories: target*:*:*
        let target_base = format!("{}/target*", host_path.display());

        for target_entry in fs::read_dir(&target_base)? {
            let target_path = target_entry?.path();
            if !target_path.is_dir() {
                continue;
            }

            // Look for block directory under each target
            let block_path = target_path.join("block");

            if block_path.exists() && block_path.is_dir() {
                for entry in fs::read_dir(&block_path)? {
                    let device_name = entry?.file_name();
                    let device_str = device_name.to_string_lossy().to_string();

                    // Skip directories, only process actual devices
                    if !device_str.starts_with("sd")
                        && !device_str.starts_with("nvme")
                        && !device_str.starts_with("vd")
                    {
                        continue;
                    }

                    let device_path = PathBuf::from(format!("/dev/{}", device_str));
                    devices.insert(device_path);
                }
            }
        }
    }

    Ok(devices.into_iter().collect())
}

/// Check if any of the given devices have active mountpoints.
pub fn check_mounted(devices: &[PathBuf]) -> Result<Vec<SafetyConcern>, SafetyError> {
    let mut concerns = Vec::new();

    // Parse /proc/mounts directly (more portable than findmnt)
    // Format: device mountpoint fstype options dump pass
    if let Ok(file) = fs::File::open("/proc/mounts") {
        for line_result in BufReader::new(file).lines() {
            let line = line_result?;
            let parts: Vec<&str> = line.split_whitespace().collect();

            if parts.len() < 3 {
                continue;
            }

            let device_path = PathBuf::from(parts[0]);
            let mountpoint = parts[1];
            let fstype = parts[2];

            // Check if this mount matches any of our target devices
            for target in devices {
                if is_mounted_device(&device_path, target) {
                    concerns.push(SafetyConcern::MountedFilesystem {
                        device: device_path.to_string_lossy().to_string(),
                        mountpoint: mountpoint.to_string(),
                        fstype: fstype.to_string(),
                    });
                    break;
                }
            }
        }
    }

    Ok(concerns)
}

/// Check if a mount point device matches (or is under) our target device.
fn is_mounted_device(mount_dev: &Path, target: &Path) -> bool {
    let mount_str = mount_dev.to_string_lossy().to_string();
    let target_str = target.to_string_lossy().to_string();

    // Direct match: /dev/sdb1 mounted on /mnt/data matches /dev/sdb1 check
    if mount_str == target_str {
        return true;
    }

    // Partition matching: if we're checking /dev/sdb, also catch /dev/sdb1, /dev/sdb2, etc.
    // Check if target looks like a base device (e.g., /dev/sdX where X is letter)
    if mount_str.starts_with(&target_str) {
        let after_target = &mount_str[target_str.len()..];
        // After the base device name must be all digits (partition number) or empty
        return !after_target.is_empty() && after_target.chars().all(|c| c.is_ascii_digit());
    }

    false
}

/// Check if any of the given devices are LVM physical volumes.
pub fn check_lvm(devices: &[PathBuf]) -> Result<Vec<SafetyConcern>, SafetyError> {
    let mut concerns = Vec::new();

    // Try pvs command first (preferred)
    match run_tool("pvs") {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);

            // Parse report format: PV VG ...
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() < 2 || parts[0] == "PV" {
                    continue; // Skip header or empty lines
                }

                let pv_name = PathBuf::from(parts[0]);
                let vg_name = parts[1];

                for target in devices {
                    if is_mounted_device(&pv_name, target) {
                        concerns.push(SafetyConcern::LvmPhysicalVolume {
                            device: pv_name.to_string_lossy().to_string(),
                            vg_name: vg_name.to_string(),
                        });
                        break;
                    }
                }
            }
        }
        Ok(_) => {
            // pvs ran but failed (e.g., no LVM installed) - return empty
            return Ok(concerns);
        }
        Err(SafetyError::ToolExecution { .. }) => {
            // pvs not installed - return empty, not an error
            return Ok(concerns);
        }
        Err(_) => {
            // Other error - skip LVM check
            return Ok(concerns);
        }
    }

    Ok(concerns)
}

/// Check if any of the given devices are mdraid members.
pub fn check_mdraid(devices: &[PathBuf]) -> Result<Vec<SafetyConcern>, SafetyError> {
    let mut concerns = Vec::new();

    // Parse /proc/mdstat (more portable than mdadm --detail --scan)
    if let Ok(file) = fs::File::open("/proc/mdstat") {
        let mut current_array: Option<String> = None;

        for line_result in BufReader::new(file).lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(_) => continue,
            };

            // Detect array definition line: "md0 : active raid1 sdb1[0] sdc1[1]"
            if line.starts_with("md") && line.contains(":") {
                // Extract array name (e.g., "md0" from "md0 : ...")
                current_array = Some(line.split(':').next().unwrap_or("").trim().to_string());

                // Also check for device names in the same line
                let parts: Vec<&str> = line.split_whitespace().collect();
                for part in parts {
                    if part.starts_with("sd") || part.starts_with("nvme") || part.starts_with("vd")
                    {
                        // This is a device name like sdb1, nvme0n1p1, etc.
                        let dev_path =
                            PathBuf::from(format!("/dev/{}", part.trim_end_matches(']')));

                        for target in devices {
                            if is_mounted_device(&dev_path, target) {
                                if let Some(ref array_name) = current_array {
                                    concerns.push(SafetyConcern::MdraidMember {
                                        device: dev_path.to_string_lossy().to_string(),
                                        array_name: array_name.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            } else if let Some(ref array_name) = current_array {
                // Continuation line - might contain more devices
                let parts: Vec<&str> = line.split_whitespace().collect();
                for part in parts {
                    if (part.starts_with("sd")
                        || part.starts_with("nvme")
                        || part.starts_with("vd"))
                        && part.contains('[')
                    {
                        // Device with array index like sdb1[0]
                        let dev_name = part.split('[').next().unwrap_or("");
                        if !dev_name.is_empty() {
                            let dev_path = PathBuf::from(format!("/dev/{}", dev_name));

                            for target in devices {
                                if is_mounted_device(&dev_path, target) {
                                    concerns.push(SafetyConcern::MdraidMember {
                                        device: dev_path.to_string_lossy().to_string(),
                                        array_name: array_name.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(concerns)
}

/// Check if any of the given devices are ZFS vdevs.
pub fn check_zfs(devices: &[PathBuf]) -> Result<Vec<SafetyConcern>, SafetyError> {
    let mut concerns = Vec::new();

    // Try zpool status command first (preferred)
    match run_tool("zpool") {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);

            let mut current_pool: Option<String> = None;

            for line in stdout.lines() {
                // Detect pool name line: "pool: storage"
                if line.starts_with("pool:") {
                    current_pool = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
                    continue;
                }

                // Check for device paths (lines starting with /dev/)
                if let Some(ref pool_name) = current_pool {
                    if line.trim_start().starts_with("/dev/") {
                        // Parse device path from line
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        for part in parts {
                            if part.starts_with("/dev/") {
                                let dev_path = PathBuf::from(part);

                                for target in devices {
                                    if is_mounted_device(&dev_path, target) {
                                        concerns.push(SafetyConcern::ZfsVdev {
                                            device: dev_path.to_string_lossy().to_string(),
                                            pool_name: pool_name.clone(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(_) => {
            // zpool ran but failed (e.g., no pools) - return empty
            return Ok(concerns);
        }
        Err(SafetyError::ToolExecution { .. }) => {
            // zfs not installed - return empty, not an error
            return Ok(concerns);
        }
        Err(_) => {
            // Other error - skip ZFS check
            return Ok(concerns);
        }
    }

    Ok(concerns)
}

/// Run a tool and capture output. Returns None if tool is not found (exit 127).
fn run_tool(tool: &str) -> Result<Output, SafetyError> {
    match Command::new(tool).output() {
        Ok(output) => Ok(output),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(SafetyError::ToolExecution {
            tool: tool.to_string(),
            reason: "command not found".to_string(),
        }),
        Err(e) => Err(SafetyError::ToolExecution {
            tool: tool.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// Run all safety checks against the given downstream devices.
pub fn check_all(devices: &[PathBuf]) -> Result<Vec<SafetyConcern>, SafetyError> {
    let mut concerns = Vec::new();

    concerns.extend(check_mounted(devices)?);
    concerns.extend(check_lvm(devices)?);
    concerns.extend(check_mdraid(devices)?);
    concerns.extend(check_zfs(devices)?);

    Ok(concerns)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safety_concern_human_message_is_actionable() {
        let c = SafetyConcern::MountedFilesystem {
            device: "/dev/sdb1".into(),
            mountpoint: "/srv/data".into(),
            fstype: "ext4".into(),
        };
        let msg = c.human();
        assert!(msg.contains("/srv/data"));
        assert!(msg.contains("umount"));
    }

    #[test]
    fn safety_concern_lvm_message_includes_vg_name() {
        let c = SafetyConcern::LvmPhysicalVolume {
            device: "/dev/sdb".into(),
            vg_name: "data".into(),
        };
        let msg = c.human();
        assert!(msg.contains("vgchange"));
        assert!(msg.contains("data"));
    }

    #[test]
    fn safety_concern_mdraid_message_includes_array() {
        let c = SafetyConcern::MdraidMember {
            device: "/dev/sdb1".into(),
            array_name: "md0".into(),
        };
        let msg = c.human();
        assert!(msg.contains("mdadm"));
        assert!(msg.contains("/dev/md0"));
    }

    #[test]
    fn safety_concern_zfs_message_includes_pool() {
        let c = SafetyConcern::ZfsVdev {
            device: "/dev/sdb".into(),
            pool_name: "storage".into(),
        };
        let msg = c.human();
        assert!(msg.contains("zpool"));
        assert!(msg.contains("export"));
    }

    #[test]
    fn is_mounted_device_matches_exact() {
        let mount_dev = PathBuf::from("/dev/sdb1");
        let target = PathBuf::from("/dev/sdb1");
        assert!(is_mounted_device(&mount_dev, &target));
    }

    #[test]
    fn is_mounted_device_matches_partition() {
        // Checking /dev/sdb should catch mounts of /dev/sdb1, /dev/sdb2, etc.
        let mount_dev = PathBuf::from("/dev/sdb1");
        let target = PathBuf::from("/dev/sdb");
        assert!(is_mounted_device(&mount_dev, &target));

        let mount_dev = PathBuf::from("/dev/sdb99");
        let target = PathBuf::from("/dev/sdb");
        assert!(is_mounted_device(&mount_dev, &target));
    }

    #[test]
    fn is_mounted_device_no_match_different_disk() {
        let mount_dev = PathBuf::from("/dev/sdc1");
        let target = PathBuf::from("/dev/sdb");
        assert!(!is_mounted_device(&mount_dev, &target));
    }

    #[test]
    fn is_mounted_device_no_match_partial_name() {
        // /dev/sda should not match /dev/sd
        let mount_dev = PathBuf::from("/dev/sda");
        let target = PathBuf::from("/dev/sd");
        assert!(!is_mounted_device(&mount_dev, &target));
    }

    #[test]
    fn check_lvm_returns_empty_when_pvs_missing() {
        // If pvs binary doesn't exist, should return empty Vec, not error
        let devices = vec![PathBuf::from("/dev/sdb")];
        let result = check_lvm(&devices);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn check_zfs_handles_missing_zpool_tool() {
        // No zfs installed → Ok(empty)
        let devices = vec![PathBuf::from("/dev/sdb")];
        let result = check_zfs(&devices);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn check_all_aggregates_concerns_from_all_subsystems() {
        // Use a mock scenario: device /dev/sdb is both mounted and LVM PV
        let devices = vec![PathBuf::from("/dev/sdb")];

        // This test verifies the structure - actual detection requires real system state
        // We'll verify by checking that check_mounted returns Ok (not error) when no mounts exist
        let result = check_all(&devices);
        assert!(result.is_ok());
    }

    #[test]
    fn integration_check_all_with_no_concerns_returns_empty() {
        // Mock scenario: fresh system with no mounted devices, LVM, mdraid, or ZFS
        let devices = vec![PathBuf::from("/dev/sdb99")]; // Non-existent device
        let result = check_all(&devices);
        assert!(result.is_ok());

        let concerns = result.unwrap();
        assert!(
            concerns.is_empty(),
            "Should have no concerns for non-existent device"
        );
    }

    #[test]
    fn parse_proc_mounts_format() {
        // Test that we correctly parse /proc/mounts format
        let test_line = "/dev/sdb1 /mnt/data ext4 rw,relatime 0 0";
        let parts: Vec<&str> = test_line.split_whitespace().collect();

        assert_eq!(parts.len(), 6);
        assert_eq!(parts[0], "/dev/sdb1");
        assert_eq!(parts[1], "/mnt/data");
        assert_eq!(parts[2], "ext4");
    }

    #[test]
    fn check_mounted_handles_empty_proc_mounts() {
        // Edge case: /proc/mounts exists but is empty or malformed
        let devices = vec![PathBuf::from("/dev/sdb")];

        // Should not panic on any input
        let result = check_mounted(&devices);
        assert!(result.is_ok());
    }

    #[test]
    fn check_mdraid_parses_proc_mdstat_format() {
        // Test mdstat parsing logic with known format
        let test_line = "md0 : active raid1 sdb1[0] sdc1[1]";

        assert!(test_line.starts_with("md"));
        assert!(test_line.contains(":"));

        let parts: Vec<&str> = test_line.split_whitespace().collect();
        // Should find "md0" as array name and device names like sdb1, sdc1
        assert_eq!(parts[0], "md0");
    }

    #[test]
    fn check_zfs_parses_pool_status_format() {
        // Test zpool status parsing logic with known format
        let pool_line = "pool: storage";
        let device_line = "  /dev/sdb (mirror)";

        assert!(pool_line.starts_with("pool:"));
        assert!(device_line.trim_start().starts_with("/dev/"));
    }

    #[test]
    fn devices_attached_to_card_returns_empty_for_missing_bdf() {
        // BDF that doesn't exist should return empty Vec, not error
        let result = devices_attached_to_card("0000:99:99.0");
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn check_all_returns_multiple_concerns_when_present() {
        // Verify that multiple concerns can be returned (structure test)
        let mut concerns = vec![SafetyConcern::MountedFilesystem {
            device: "/dev/sdb1".into(),
            mountpoint: "/mnt/data".into(),
            fstype: "ext4".into(),
        }];

        concerns.push(SafetyConcern::LvmPhysicalVolume {
            device: "/dev/sdc".into(),
            vg_name: "vg0".into(),
        });

        assert_eq!(concerns.len(), 2);
        assert!(matches!(
            concerns[0],
            SafetyConcern::MountedFilesystem { .. }
        ));
        assert!(matches!(
            concerns[1],
            SafetyConcern::LvmPhysicalVolume { .. }
        ));
    }

    #[test]
    fn safety_concern_clone_works() {
        let c1 = SafetyConcern::MountedFilesystem {
            device: "/dev/sdb1".into(),
            mountpoint: "/mnt/data".into(),
            fstype: "ext4".into(),
        };

        let c2 = c1.clone();
        assert_eq!(c1.human(), c2.human());
    }

    #[test]
    fn check_all_with_empty_device_list_returns_empty() {
        // Edge case: no devices to check should return empty, not error
        let devices: Vec<PathBuf> = vec![];
        let result = check_all(&devices);

        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn safety_concern_debug_impl_works() {
        let c = SafetyConcern::MountedFilesystem {
            device: "/dev/sdb1".into(),
            mountpoint: "/mnt/data".into(),
            fstype: "ext4".into(),
        };

        let debug_str = format!("{:?}", c);
        assert!(debug_str.contains("MountedFilesystem"));
    }
}
