// SPDX-License-Identifier: AGPL-3.0-or-later
//! The password of a sandbox boot and the files it lives in.

use std::io;
use std::path::{Path, PathBuf};

use super::EditorConfig;

/// Bytes of randomness in the password (hex: twice as many characters).
pub(super) const PASSWORD_BYTES: usize = 32;

/// Where the kernel keeps the id of the running boot.
pub const BOOT_ID_FILE: &str = "/proc/sys/kernel/random/boot_id";

/// The boot id in `path`, or `None` when it cannot be read (then no password is kept).
pub(super) fn boot_id(path: &Path) -> Option<String> {
    let id = std::fs::read_to_string(path).ok()?;
    let id = id.trim();
    (!id.is_empty() && id.len() <= 64).then(|| id.to_owned())
}

/// The file beside the password that names the boot it was made in.
pub(super) fn boot_stamp(password_file: &Path) -> PathBuf {
    let mut stamp = password_file.as_os_str().to_owned();
    stamp.push(".boot");
    PathBuf::from(stamp)
}

/// The password of this boot: the one in `config.password_file` when it was made in the boot
/// `config.boot_id` names, else a new one, written there.
pub(super) fn boot_password(config: &EditorConfig) -> io::Result<String> {
    let boot = boot_id(&config.boot_id);
    let stamp = boot_stamp(&config.password_file);
    let made_in_this_boot = boot.is_some() && std::fs::read_to_string(&stamp).ok() == boot;
    if made_in_this_boot && let Some(password) = existing_password(&config.password_file) {
        return Ok(password);
    }
    let password = new_password()?;
    write_password(&config.password_file, &password)?;
    // An unknown boot leaves an empty stamp, which no boot matches.
    write_private(&stamp, boot.as_deref().unwrap_or_default())?;
    Ok(password)
}

/// A fresh random password, lowercase hex.
///
/// # Errors
///
/// When the system has no randomness (the server must not start with a guessable password).
pub fn new_password() -> io::Result<String> {
    let mut bytes = [0_u8; PASSWORD_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
    Ok(puddle_types::hex_lower(&bytes))
}

/// The password already in `path`, when it is one this module wrote.
pub(super) fn existing_password(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let valid = text.len() == PASSWORD_BYTES * 2 && text.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then_some(text)
}

/// Writes `password` to `path` for its owner only, through a temporary file renamed over it, so a
/// reader never sees half of it or an older file's mode.
///
/// # Errors
///
/// When the folder or the file can't be written.
pub fn write_password(path: &Path, password: &str) -> io::Result<()> {
    write_private(path, password)
}

/// Writes `text` to `path` for its owner only, through a temporary file renamed over it.
pub(super) fn write_private(path: &Path, text: &str) -> io::Result<()> {
    puddle_fs::private::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests;
