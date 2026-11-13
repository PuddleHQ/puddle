// SPDX-License-Identifier: AGPL-3.0-or-later
//! The crate's only unsafe code: creating a pipe instance with an explicit security descriptor.
//! The user's SID and descriptor helpers live in `puddle_fs::win`.

use std::ffi::{OsStr, c_void};
use std::io;

use puddle_fs::win::LocalBox;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// Creates one pipe instance whose security descriptor is `sddl`.
pub(super) fn create_pipe(
    options: &ServerOptions,
    path: &OsStr,
    sddl: &str,
) -> io::Result<NamedPipeServer> {
    let descriptor = LocalBox::security_descriptor(sddl)?;
    let mut attrs = descriptor.attributes();
    // SAFETY: `attrs` is a valid SECURITY_ATTRIBUTES for the whole call, and the descriptor it
    // points at (`descriptor`) is freed only after the call returns.
    unsafe { options.create_with_security_attributes_raw(path, (&raw mut attrs).cast::<c_void>()) }
}

#[cfg(test)]
mod tests;
