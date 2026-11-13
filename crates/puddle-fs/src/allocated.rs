// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub fn allocated_bytes(path: &Path) -> io::Result<u64> {
    // The two bodies have their own names so that a mutation exclusion can name the Windows one,
    // which a Linux run never compiles.
    #[cfg(unix)]
    {
        allocated_unix(path)
    }
    #[cfg(windows)]
    {
        allocated_windows(path)
    }
}

#[cfg(unix)]
fn allocated_unix(path: &Path) -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    // `blocks` counts 512-byte units whatever the file system's block size is.
    Ok(std::fs::metadata(path)?.blocks().saturating_mul(512))
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetCompressedFileSizeW is the only way to read a sparse file's allocated size"
)]
fn allocated_windows(path: &Path) -> io::Result<u64> {
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
mod tests;
