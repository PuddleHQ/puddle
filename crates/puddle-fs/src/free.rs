// SPDX-License-Identifier: AGPL-3.0-or-later
//! How much room is left on the drive that holds a folder.

use std::io;
use std::path::Path;

/// Bytes a normal user can still write on the drive (volume, file system) that holds `path`.
/// This is the figure `df` and Explorer show as free, so it leaves out what the file system keeps
/// back for the superuser and counts quota where the OS applies one. A folder that does not exist
/// yet is measured by its nearest existing parent, which is the drive it will be created on.
///
/// # Errors
///
/// No folder on the path exists, or the OS refuses to say.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let existing = path
        .ancestors()
        .find(|p| !p.as_os_str().is_empty() && p.exists())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} and its parents do not exist", path.display()),
            )
        })?;
    // The two bodies have their own names so that a mutation exclusion can name the Windows one,
    // which a Linux run never compiles.
    #[cfg(unix)]
    {
        free_bytes_unix(existing)
    }
    #[cfg(windows)]
    {
        free_bytes_windows(existing)
    }
}

#[cfg(unix)]
fn free_bytes_unix(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetDiskFreeSpaceExW is the only call that reports the space free to this user"
)]
fn free_bytes_windows(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0_u64;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call, `free` is a valid
    // place for the answer, and the two figures this does not ask for may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &raw mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(free)
}

#[cfg(test)]
mod tests;
