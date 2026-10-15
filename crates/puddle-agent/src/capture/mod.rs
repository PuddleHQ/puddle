// SPDX-License-Identifier: GPL-3.0-or-later
//! Capture for tools that ignore the proxy settings.
//!
//! A client that doesn't use `HTTPS_PROXY` resolves a name and connects to the answer. The agent
//! answers the name itself ([`dns`]) with a *stand-in* address from `198.18.0.0/15`
//! ([`table`]), after asking the host whether the name is worth one. The connection to that
//! address is then redirected to the agent, which looks the name up in the table again and
//! sends `CONNECT name:port` to the host's proxy, exactly as an explicit proxy client would, so
//! the rules, the toggles and the audit see the name.
//!
//! - [`rules`]: the redirect and refusal rules as data; [`nft`] installs them over netlink and
//!   [`guard`] keeps them in place; [`link`] makes the interface they rely on;
//! - [`listen`]: the listener the redirect sends connections to;
//! - [`udp`]: reports the UDP the rules refused;
//! - [`dns`]: the stub DNS server, its cache and its `resolve` lookups on the host;
//! - [`wire`]: the DNS messages it reads and writes;
//! - [`table`]: stand-in address ⇄ name, kept across agent restarts;
//! - [`names`]: which names are answered locally.

pub mod dns;
#[cfg(target_os = "linux")]
pub mod guard;
#[cfg(target_os = "linux")]
pub mod link;
#[cfg(target_os = "linux")]
pub mod listen;
pub mod names;
#[cfg(target_os = "linux")]
mod netlink;
#[cfg(target_os = "linux")]
pub mod nft;
pub mod rules;
pub mod table;
pub mod udp;
pub mod wire;
