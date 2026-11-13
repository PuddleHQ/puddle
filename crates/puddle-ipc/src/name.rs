// SPDX-License-Identifier: AGPL-3.0-or-later
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
