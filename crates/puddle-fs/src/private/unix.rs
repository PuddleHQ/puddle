// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unix: modes `0700` for folders Puddle creates, `0600` for files.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use super::{CheckError, Exposed};

pub(super) fn create_dir(dir: &Path) -> io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    DirBuilder::new().recursive(true).mode(0o700).create(dir)
}

pub(super) fn create_new_dir(dir: &Path) -> io::Result<()> {
    // `mkdir` fails on an existing path and never follows a symlink in the last component.
    DirBuilder::new().mode(0o700).create(dir)?;
    // The umask can only remove bits from 0700; set it exactly anyway, then check (a file system
    // that ignores `chmod` would otherwise leave a folder that isn't what it claims to be).
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    expect_mode(dir, 0o700)
}

/// Fails unless `path` (not followed, if a symlink) has exactly `mode`.
fn expect_mode(path: &Path, mode: u32) -> io::Result<()> {
    let actual = fs::symlink_metadata(path)?.permissions().mode() & 0o7777;
    if actual == mode {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("expected mode {mode:o}, found {actual:o}"),
        ))
    }
}

pub(super) fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

pub(super) fn create_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

pub(super) fn check(file: &File) -> Result<(), CheckError> {
    let mode = file.metadata()?.permissions().mode() & 0o777;
    // No group or other bit: the low six bits are clear.
    if mode.trailing_zeros() >= 6 {
        Ok(())
    } else {
        Err(CheckError::Exposed(Exposed::Mode(mode)))
    }
}

pub(super) fn tighten_dir(dir: &Path) -> io::Result<Option<Exposed>> {
    let meta = fs::metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a folder"));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode.trailing_zeros() >= 6 {
        return Ok(None);
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let now = fs::metadata(dir)?.permissions().mode() & 0o777;
    if now & 0o077 != 0 {
        return Err(io::Error::other(format!("the mode is still {now:o}")));
    }
    Ok(Some(Exposed::Mode(mode)))
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
    let file = File::open(path)?;
    match check(&file) {
        Ok(()) => Ok(None),
        Err(CheckError::Io(err)) => Err(err),
        Err(CheckError::Exposed(was)) => {
            // Through the open handle, so a swap of the path after the check changes nothing.
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            check(&file).map_err(io::Error::other)?;
            Ok(Some(was))
        }
    }
}

#[cfg(test)]
mod tests;
