// SPDX-License-Identifier: GPL-3.0-or-later
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
    free_bytes_of(existing)
}

#[cfg(unix)]
fn free_bytes_of(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "GetDiskFreeSpaceExW is the only call that reports the space free to this user"
)]
fn free_bytes_of(path: &Path) -> io::Result<u64> {
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
mod tests {
    use super::*;

    #[test]
    fn a_folder_that_exists_has_free_space_and_a_new_one_has_its_parents() {
        let dir = tempfile::tempdir().unwrap();
        let here = free_bytes(dir.path()).unwrap();
        assert!(here > 0, "a temp folder's drive has some room");
        let later = free_bytes(&dir.path().join("not").join("yet")).unwrap();
        // The same drive, so the two reads differ only by what else is writing: far less than 1 GiB.
        assert!(here.abs_diff(later) < 1 << 30, "{here} against {later}");
    }

    #[test]
    fn a_path_with_no_existing_parent_is_not_found() {
        let error = free_bytes(Path::new("")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
