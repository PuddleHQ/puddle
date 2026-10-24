// SPDX-License-Identifier: GPL-3.0-or-later
//! How much disk a file really takes. A VM's disk image is sparse: its length is the size of the
//! disk, and the host only stores what the guest wrote. The length says nothing about use.

use std::io;
use std::path::Path;

/// Bytes the host's file system has allocated for the file at `path`: for a sparse file the part
/// that holds data, not its length.
///
/// # Errors
///
/// The file cannot be looked at (it is missing, or the process may not read its entry).
#[cfg(unix)]
pub fn allocated_bytes(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    // `blocks` counts 512-byte units whatever the file system's block size is.
    Ok(std::fs::metadata(path)?.blocks().saturating_mul(512))
}

/// Bytes the host's file system has allocated for the file at `path`: for a sparse file the part
/// that holds data, not its length.
///
/// # Errors
///
/// The file cannot be looked at (it is missing, or the process may not read its entry).
#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetCompressedFileSizeW is the only way to read a sparse file's allocated size"
)]
pub fn allocated_bytes(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::GetCompressedFileSizeW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut high = 0_u32;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, and `high` is a
    // valid place for the high half of the size.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &raw mut high) };
    if low == u32::MAX {
        // The low half can be all ones in a valid size; only the last error tells the two apart.
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(0) {
            return Err(error);
        }
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    #[test]
    fn a_file_with_data_takes_at_least_its_data_and_a_missing_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.raw");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![7_u8; 3 * 1024 * 1024]).unwrap();
        file.sync_all().unwrap();
        let used = allocated_bytes(&path).unwrap();
        assert!(used >= 3 * 1024 * 1024, "{used}");
        assert!(used < 4 * 1024 * 1024 + 65_536, "{used}");
        let error = allocated_bytes(&dir.path().join("missing")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    /// Where the file system makes holes (every Linux one under test; not NTFS without the sparse
    /// flag), a long file with a little data takes far less than its length.
    #[cfg(unix)]
    #[test]
    fn a_sparse_file_takes_what_was_written_not_its_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.raw");
        let mut file = std::fs::File::create(&path).unwrap();
        file.set_len(1024 * 1024 * 1024).unwrap();
        file.write_all(&[1_u8; 4096]).unwrap();
        file.sync_all().unwrap();
        let used = allocated_bytes(&path).unwrap();
        assert!(used >= 4096, "{used}");
        assert!(
            used < 64 * 1024 * 1024,
            "the file system made it dense: {used}"
        );
    }
}
