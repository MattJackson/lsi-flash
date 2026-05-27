//! Persistent BAR1 mmap for RealIoc.
//!
//! The existing `Platform::mmap_ro` returns a heap COPY of the mapped bytes
//! (`Vec<u8>::into_boxed_slice` after `munmap`) — useful for snapshot reads
//! but UNUSABLE for register access where writes must reach hardware and
//! reads must reflect chip state at the moment of access.
//!
//! `MmapRegion` keeps the `mmap()` alive for its lifetime and `munmap()`s
//! on Drop. Writes through `as_mut_slice()` go directly to the chip register
//! file (e.g., `/sys/bus/pci/devices/0000:03:00.0/resource1`).
//!
//! Linux-only — the BAR1 sysfs convention does not exist on other platforms.

#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;
#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(target_os = "linux")]
use std::ptr;

/// A live `mmap()` region. Drops `munmap()`.
///
/// Construct via `MmapRegion::open_rw(path, len)`. The returned region owns
/// the mapping for its lifetime. Holding `&mut` is required for write access.
#[cfg(target_os = "linux")]
pub struct MmapRegion {
    ptr: *mut u8,
    len: usize,
}

#[cfg(target_os = "linux")]
impl MmapRegion {
    /// Open the file at `path` and mmap `len` bytes from offset 0 with
    /// PROT_READ | PROT_WRITE and MAP_SHARED. Writes are visible to other
    /// processes mapping the same file (i.e., reach hardware for BAR1).
    pub fn open_rw(path: &Path, len: usize) -> io::Result<Self> {
        let fd = File::options().read(true).write(true).open(path)?;
        let ptr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // fd can drop now; the mmap holds an independent reference.
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[cfg(target_os = "linux")]
impl Drop for MmapRegion {
    fn drop(&mut self) {
        if !self.ptr.is_null() && self.ptr != libc::MAP_FAILED as *mut u8 {
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}

// Safety: the mmap'd region is shared memory; multiple threads CAN access
// it but the caller is responsible for synchronization (BAR1 register
// semantics are not Send-safe in general — only RealIoc<P> with &mut should
// hold this). Marking Send so RealIoc can be moved between threads if a
// caller holds it exclusively.
#[cfg(target_os = "linux")]
unsafe impl Send for MmapRegion {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn mmap_region_open_rw_against_tempfile() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[0xAB; 4096]).unwrap();
        tmp.flush().unwrap();

        let region = MmapRegion::open_rw(tmp.path(), 4096).unwrap();
        assert_eq!(region.len(), 4096);
        assert_eq!(region.as_slice()[0], 0xAB);
        assert_eq!(region.as_slice()[4095], 0xAB);
    }

    #[test]
    fn mmap_region_write_visible_through_slice() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(&[0u8; 4096]).unwrap();
        tmp.flush().unwrap();

        let mut region = MmapRegion::open_rw(tmp.path(), 4096).unwrap();
        region.as_mut_slice()[0] = 0xDE;
        region.as_mut_slice()[1] = 0xAD;
        // Read back through the same region — proves the write landed
        assert_eq!(region.as_slice()[0], 0xDE);
        assert_eq!(region.as_slice()[1], 0xAD);
    }

    #[test]
    fn mmap_region_open_nonexistent_returns_err() {
        let result = MmapRegion::open_rw(Path::new("/nonexistent/bar1"), 4096);
        assert!(result.is_err());
    }
}
