// SPDX-License-Identifier: AGPL-3.0-or-later
//! Windows: an explicit ACL with one entry, full control for the current user's SID, and
//! protected from inheritance, so neither the parent's entries (administrators, `SYSTEM`) nor
//! Everyone come back. It is set when the file or folder is created, so there is no window with
//! the inherited ACL. Folders carry inheritable entries, so files created inside by other means
//! are owner-only too.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;

use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;

use super::{CheckError, Exposed};
use crate::win;

pub(super) fn create_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    // Missing ancestors, outermost first, each created with the owner-only ACL.
    let mut missing: Vec<&Path> = dir.ancestors().take_while(|p| !p.is_dir()).collect();
    missing.retain(|p| !p.as_os_str().is_empty());
    for path in missing.into_iter().rev() {
        match win::create_owner_only_dir(path) {
            Ok(()) => {}
            // Someone else created it meanwhile; fine if it is a folder now.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

pub(super) fn create_new_dir(dir: &Path) -> io::Result<()> {
    // `CreateDirectoryW` fails with `AlreadyExists` on any existing path.
    win::create_owner_only_dir(dir)
}

pub(super) fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    win::replace_file(from, to)
}

pub(super) fn create_file(path: &Path) -> io::Result<File> {
    win::create_owner_only_file(path)
}

pub(super) fn check(file: &File) -> Result<(), CheckError> {
    win::dacl_is_owner_only(file)?.map_err(|why| CheckError::Exposed(Exposed::Acl(why)))
}

pub(super) fn tighten_dir(dir: &Path) -> io::Result<Option<Exposed>> {
    if !fs::metadata(dir)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    let open = || {
        // Backup semantics is what lets a folder be opened as a handle.
        OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
    };
    let Err(was) = win::dacl_is_owner_only(&open()?)? else {
        return Ok(None);
    };
    win::set_owner_only_acl(dir, true)?;
    win::dacl_is_owner_only(&open()?)?.map_err(io::Error::other)?;
    Ok(Some(Exposed::Acl(was)))
}

pub(super) fn tighten_file(path: &Path) -> io::Result<Option<Exposed>> {
    // A missing file is `NotFound` here and at every later step; the caller reads it as "nothing
    // to tighten".
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let Err(was) = win::dacl_is_owner_only(&File::open(path)?)? else {
        return Ok(None);
    };
    win::set_owner_only_acl(path, false)?;
    win::dacl_is_owner_only(&File::open(path)?)?.map_err(io::Error::other)?;
    Ok(Some(Exposed::Acl(was)))
}

#[cfg(test)]
mod tests;
