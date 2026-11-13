// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recognising an SSH client by the first bytes it sends.
//!
//! An SSH client starts with its identification line (RFC 4253 section 4.2):
//! `SSH-<protocol version>-<software version>`, whatever the port. That line, not the port,
//! tells the proxy a connection is SSH: `ssh.github.com:443` is SSH and some other service on
//! port 22 is not.
//!
//! A tool that carries an SSH client's bytes to the proxy (the agent's `connect` command, the
//! `ssh` `ProxyCommand`) puts [`PROTOCOL_HEADER`]: [`SSH`] on its `CONNECT` once it has seen the
//! line, so the proxy can refuse before it decides anything about the destination. The proxy also
//! looks at the first bytes itself ([`is_banner`]) for a client that sends them with the `CONNECT`
//! or inside an established tunnel.
//!
//! The header only ever makes the proxy stricter: a guest that sets it wrongly gets its own
//! connection refused, and no connection is allowed because of it.

/// The request header that says the tunnel will carry SSH.
pub const PROTOCOL_HEADER: &str = "x-puddle-protocol";

/// The value of [`PROTOCOL_HEADER`] for SSH.
pub const SSH: &str = "ssh";

/// Fewest bytes [`is_banner`] needs: `SSH-` and the first digit of the protocol version.
pub const BANNER_MIN: usize = 5;

/// Whether `bytes` begin an SSH identification line: `SSH-` followed by a digit.
///
/// Fewer than [`BANNER_MIN`] bytes never match; a caller that has less reads on first.
///
/// ```
/// use puddle_agent_proto::ssh::is_banner;
/// assert!(is_banner(b"SSH-2.0-OpenSSH_9.6\r\n"));
/// assert!(is_banner(b"SSH-1.99-libssh2_1.11.0"));
/// assert!(!is_banner(b"SSH-"));
/// assert!(!is_banner(b"\x16\x03\x01\x02\x00"));
/// assert!(!is_banner(b"GET / HTTP/1.1\r\n"));
/// ```
#[must_use]
pub fn is_banner(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SSH-") && bytes.get(4).is_some_and(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests;
