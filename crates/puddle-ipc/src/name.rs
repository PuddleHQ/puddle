// SPDX-License-Identifier: GPL-3.0-or-later
//! Unguessable endpoint names.
//!
//! On Windows the pipe namespace is shared by every user on the machine and anyone can list it, so
//! the name carries no workspace name (it would tell other users which workspaces exist) and 128
//! random bits, so nobody can create it first except by listing it after Puddle did, which
//! `FILE_FLAG_FIRST_PIPE_INSTANCE` then catches.

use crate::IpcError;

/// A fresh random name of `N` bytes as `2 * N` lowercase hex digits.
///
/// Pipe names use 16 bytes (128 bits: unguessable in the shared namespace). Unix names use 8:
/// they live in a private directory, so they only need to be unique, and the socket path limit
/// (`sun_path`, about 107 bytes) leaves little room.
pub(crate) fn random_name<const N: usize>() -> Result<String, IpcError> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(IpcError::Random)?;
    Ok(hex(&bytes))
}

/// The longest endpoint name: a workspace id.
const MAX_NAME_LEN: usize = 63;

/// Whether `name` is 1 to [`MAX_NAME_LEN`] characters of `a-z`, `0-9` and `-`: a name that is safe
/// in a file name and a pipe name on every OS.
pub(crate) fn check_name(name: &str) -> Result<(), IpcError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(IpcError::InvalidName {
            name: name.to_owned(),
        })
    }
}

/// Whether `id` is what `random_name::<16>` makes: 32 lowercase hex digits.
pub(crate) fn is_install_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            // Writing to a String can't fail.
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests;
