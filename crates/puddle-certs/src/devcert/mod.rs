// SPDX-License-Identifier: GPL-3.0-or-later
//! The localhost development certificate (HS-23..HS-25).
//!
//! A server a developer runs in a workspace over HTTPS reaches the host's browser as raw TCP on
//! a forwarded port, so the browser accepts the server's certificate only if the host trusts it.
//!
//! | Step | Piece |
//! |---|---|
//! | an existing ASP.NET development certificate the user's Windows trusts, with an exportable key, is reused: no dialog | [`DevCertTrust::existing`], choice in [`SkipReason`] |
//! | otherwise one leaf per host user (`CA:false`, loopback names, ASP.NET marker, 365 days, replaced under 30) in the owner-only data folder | [`DevCertManager::ensure`], [`leaf`] |
//! | that leaf is trusted once in the user's `Root` store (one Windows confirmation, never overriding a policy) | [`DevCertTrust::trust`] |
//! | where it stands, for the first-run line | [`DevCertStatus`] |
//!
//! No certificate authority key exists anywhere: the leaf cannot sign, and the only key that
//! reaches a workspace is the leaf's. The per-workspace CA of the credential proxy is a different certificate
//! and is never mixed with this one.

pub mod leaf;
mod manager;
mod material;
mod reuse;
mod trust;

pub use manager::{DevCertError, DevCertManager, DevCertReport, DevCertStatus, FOLDER, LEAF_FILE};
pub use material::{DevCertMaterial, Origin, PrivateKey};
pub use reuse::{SkipReason, SkippedExisting};
pub use trust::{DevCertTrust, ExistingCert, KeyAccess, TrustError, TrustOutcome, Unsupported};

#[cfg(windows)]
pub use crate::platform::WindowsTrust;

/// The trust implementation of this host: Windows' certificate stores, or [`Unsupported`].
#[cfg(windows)]
pub type HostTrust = WindowsTrust;
/// The trust implementation of this host: Windows' certificate stores, or [`Unsupported`].
#[cfg(not(windows))]
pub type HostTrust = Unsupported;
