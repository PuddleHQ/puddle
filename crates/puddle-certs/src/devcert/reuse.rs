// SPDX-License-Identifier: GPL-3.0-or-later
//! Which existing ASP.NET development certificate Puddle reuses (HS-24).
//!
//! A certificate is reused only if it carries the marker, is a leaf, is valid now and for at
//! least [`RENEW_WITHIN`] more, is valid for loopback names only, has a private key that can be exported, and is already trusted.
//! Every certificate with the marker that falls short is reported with the first reason. The
//! best of the rest is the highest marker version, then the latest expiry.

use std::fmt;
use std::time::SystemTime;

use crate::select::Fingerprint;

use super::leaf::{RENEW_WITHIN, ShapeError, check_names, inspect, unix_secs};
use super::trust::{ExistingCert, KeyAccess};

/// Why an existing development certificate is not reused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SkipReason {
    /// It is a certificate authority.
    NotALeaf,
    /// It is valid for a name outside the localhost set (the first such name): a key that could
    /// impersonate that must not go into a workspace.
    NamesTooWide(String),
    /// Its validity has ended.
    Expired,
    /// Its validity has not begun.
    NotYetValid,
    /// Less than 30 days of validity are left.
    ExpiresSoon,
    /// There is no private key for it.
    NoKey,
    /// The key cannot be exported (and why).
    NotExportable(String),
    /// The system does not trust it.
    NotTrusted,
    /// Exporting the key failed (and why).
    ExportFailed(String),
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotALeaf => f.write_str("it is a certificate authority"),
            Self::NamesTooWide(name) => {
                write!(f, "valid for {name}, which is not a localhost name")
            }
            Self::Expired => f.write_str("expired"),
            Self::NotYetValid => f.write_str("not valid yet"),
            Self::ExpiresSoon => f.write_str("expires within 30 days"),
            Self::NoKey => f.write_str("no private key"),
            Self::NotExportable(why) => write!(f, "key can't be exported: {why}"),
            Self::NotTrusted => f.write_str("not trusted"),
            Self::ExportFailed(why) => write!(f, "exporting the key failed: {why}"),
        }
    }
}

/// An ASP.NET development certificate that is not reused, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedExisting {
    /// Which certificate.
    pub fingerprint: Fingerprint,
    /// The marker version it carries.
    pub marker_version: u8,
    /// Why it is passed over.
    pub reason: SkipReason,
}

/// The outcome of looking over the existing certificates.
#[derive(Debug, Default)]
pub(crate) struct Ranking<'a> {
    /// The certificates that qualify, best first.
    pub(crate) eligible: Vec<&'a ExistingCert>,
    /// The ones with the marker that do not.
    pub(crate) skipped: Vec<SkippedExisting>,
}

pub(crate) fn rank(existing: &[ExistingCert], now: SystemTime) -> Ranking<'_> {
    let now = unix_secs(now);
    let renew = i64::try_from(RENEW_WITHIN.as_secs()).unwrap_or(i64::MAX);
    let mut ranking = Ranking::default();
    let mut eligible = Vec::new();
    for cert in existing {
        // Not a readable certificate, or not a development certificate: not ours to report.
        let Ok(info) = inspect(&cert.der) else {
            continue;
        };
        let Some(marker_version) = info.marker_version else {
            continue;
        };
        let reason = if info.is_ca() {
            Some(SkipReason::NotALeaf)
        } else if let Err(ShapeError::ForeignName(name)) = check_names(&info) {
            Some(SkipReason::NamesTooWide(name))
        } else if info.not_after <= now {
            Some(SkipReason::Expired)
        } else if info.not_before > now {
            Some(SkipReason::NotYetValid)
        } else if info.not_after - now < renew {
            Some(SkipReason::ExpiresSoon)
        } else {
            match &cert.key {
                KeyAccess::Missing => Some(SkipReason::NoKey),
                KeyAccess::NotExportable(why) => Some(SkipReason::NotExportable(why.clone())),
                KeyAccess::Exportable if !cert.trusted => Some(SkipReason::NotTrusted),
                KeyAccess::Exportable => None,
            }
        };
        match reason {
            Some(reason) => ranking.skipped.push(SkippedExisting {
                fingerprint: Fingerprint::of(&cert.der),
                marker_version,
                reason,
            }),
            None => eligible.push((marker_version, info.not_after, cert)),
        }
    }
    eligible.sort_by_key(|&(version, not_after, _)| std::cmp::Reverse((version, not_after)));
    ranking.eligible = eligible.into_iter().map(|(_, _, cert)| cert).collect();
    ranking
}
