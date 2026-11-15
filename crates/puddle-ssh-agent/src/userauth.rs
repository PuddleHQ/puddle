// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one thing the filtered agent signs: an SSH user-authentication request (RFC 4252 section
//! 7), the data a client signs to log in with a public key.

use crate::wire;

const MSG_USERAUTH_REQUEST: u8 = 50;
const PUBLICKEY: &[u8] = b"publickey";
/// OpenSSH's method that also names the server's host key, so a signature for one server cannot
/// be used at another.
const PUBLICKEY_HOSTBOUND: &[u8] = b"publickey-hostbound-v00@openssh.com";

/// The magic at the start of an `SSHSIG` blob (a Git commit or file signature).
const SSHSIG_MAGIC: &[u8] = b"SSHSIG";

/// What the data to sign turned out to be.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Signed<'a> {
    /// A login: the session it is for, the key it names and, for the host-bound method, the
    /// server's host key.
    Login {
        /// The session identifier at the front of the data.
        session_id: &'a [u8],
        /// The public key blob the login names.
        key: &'a [u8],
        /// The server's host key blob, for `publickey-hostbound-v00@openssh.com`.
        host_key: Option<&'a [u8]>,
    },
    /// An `SSHSIG` request: a commit or file signature.
    SshSig,
    /// Anything else.
    Other,
}

/// Reads `data` as a sign request's payload.
#[must_use]
pub(crate) fn read(data: &[u8]) -> Signed<'_> {
    if data.starts_with(SSHSIG_MAGIC) {
        return Signed::SshSig;
    }
    login(data).unwrap_or(Signed::Other)
}

fn login(mut data: &[u8]) -> Option<Signed<'_>> {
    let session_id = wire::take_string(&mut data).ok()?;
    let (&kind, rest) = data.split_first()?;
    data = rest;
    if kind != MSG_USERAUTH_REQUEST {
        return None;
    }
    let _user = wire::take_string(&mut data).ok()?;
    if wire::take_string(&mut data).ok()? != b"ssh-connection" {
        return None;
    }
    let method = wire::take_string(&mut data).ok()?;
    let hostbound = match method {
        PUBLICKEY => false,
        PUBLICKEY_HOSTBOUND => true,
        _ => return None,
    };
    let (&has_signature, rest) = data.split_first()?;
    data = rest;
    if has_signature != 1 {
        return None;
    }
    let _algorithm = wire::take_string(&mut data).ok()?;
    let key = wire::take_string(&mut data).ok()?;
    let host_key = if hostbound {
        Some(wire::take_string(&mut data).ok()?)
    } else {
        None
    };
    data.is_empty().then_some(Signed::Login {
        session_id,
        key,
        host_key,
    })
}

#[cfg(test)]
mod tests;
