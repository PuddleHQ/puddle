// SPDX-License-Identifier: GPL-3.0-or-later
//! The destination guard: which destinations a workspace may reach, decided
//! before any connection. Pure policy, no network I/O; the proxy (`puddle-proxy`) calls it.
//!
//! | Stage | Item | What |
//! |---|---|---|
//! | name | [`normalise_host`], [`Target`] | strict normal form: lower case, IDNA to ASCII, LDH labels, canonical IP literals; non-canonical numbers refused |
//! | name | [`NetPolicy::check_target`] | literals and `localhost`/metadata names blocked before the rules, so a block never becomes a pending row |
//! | address | [`classify_ip`], [`AddressClass`] | public, a [`LocalCategory`] (with the rule that put it there), or one of Puddle's own endpoints |
//! | address | [`NetPolicy::check_address`], [`AddressVerdict`] | allow, exact-allow only (toggle on), or block naming the toggle |
//! | company | [`CompanySignals`], [`StaticSignals`], [`AddressClass::Company`] | the company-network category: what the host reads (proxy script, DNS suffixes, private-access clients, routes) and how it puts a private address there; empty until the host reads a signal (the shipped host reads none yet) |
//! | settings | [`LocalAccess`], [`LocalAccessSource`] | one workspace's toggles and "wildcards reach local addresses", from `puddle-settings` |
//! | endpoints | [`PuddleEndpoints`], [`EndpointKind`] | the registry every Puddle listener joins when it binds, and the names and addresses of a reverse proxy on another machine |
//! | pki | [`PkiDistributionPoints`], [`NetPolicy::pki_address`] | the certificate distribution points the host's synced roots name: plain-HTTP `GET`/`HEAD` of those URLs on a company or private address needs no rule and no toggle |
//! | message | [`block_message`] | the `403` text: what was found and which toggle would allow it; for SSH, the refusal and (on a Git host) the HTTPS form of the remote |
//!
//! The rules it applies:
//!
//! - **Local destinations** are not forbidden, only off by default: one toggle per
//!   [`LocalCategory`], with a global default and a per-workspace override.
//! - **A toggle only *permits* its category;** the destination still needs an allow rule
//!   or an approval.
//! - **Exact allows (R-14):** with the toggle on, only an **exact** allow (of the name, or of the
//!   address itself) reaches a local address; a wildcard (suffix) allow doesn't, unless
//!   "wildcards reach local addresses" is on ([`AddressVerdict::ExactOnly`]).
//! - **Puddle's own listeners** are their own class, blocked whatever the toggles say.
//! - A blocked request names the toggle that would allow it ([`block_message`]).
#![forbid(unsafe_code)]

mod access;
mod classify;
mod company;
mod endpoints;
mod guard;
mod name;
mod pki;

pub use access::{LocalAccess, LocalAccessSource};
pub use classify::{AddressClass, classify_ip, classify_with_signals};
pub use company::{CompanyFound, CompanySignals, LanReading, NoSignals, StaticSignals};
pub use endpoints::{
    EndpointKind, HostAddrs, NameRegistration, OwnAddresses, PuddleEndpoints, Registration,
};
pub use guard::{
    AZURE_DEVOPS_SSH_HOST, AddressVerdict, NetPolicy, azure_devops_ssh_message, block_message,
    company_network_message, git_https_remote,
};
pub use name::{MAX_RAW_HOST_LEN, NameError, Target, normalise_host};
pub use pki::PkiDistributionPoints;
pub use puddle_types::{CompanySignal, LocalCategory};
