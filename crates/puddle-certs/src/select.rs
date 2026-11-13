// SPDX-License-Identifier: GPL-3.0-or-later
//! Which certificates of a [`StoreSnapshot`] the guest gets.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};

use crate::der;
use crate::store::{StoreName, StoreSnapshot, StoreSource, UnreadableStore};

/// Whether a synced certificate is a trust anchor or an intermediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CertKind {
    /// From a `Root` store.
    Root,
    /// From a `CA` store, issued by a synced certificate (a corporate issuing CA that proxies
    /// often leave out of the chain they send).
    Intermediate,
}

/// Why a certificate from the stores is not synced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum SkipReason {
    /// It is in (or shares its key with a certificate in) a `Disallowed` store.
    Disallowed,
    /// Its `notAfter` has passed.
    Expired,
    /// It is not a readable X.509 certificate.
    Unreadable,
    /// An intermediate whose issuer is not among the synced certificates (e.g. a public CA's
    /// intermediate Windows cached; the guest's distro bundle covers its root).
    NotIssuedBySyncedCert,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disallowed => "distrusted by Windows (Disallowed store)",
            Self::Expired => "expired",
            Self::Unreadable => "not a readable certificate",
            Self::NotIssuedBySyncedCert => "intermediate not issued by a synced root",
        })
    }
}

/// SHA-256 of a certificate's DER, the identity used for duplicates and guest file names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// The fingerprint of `der`.
    #[must_use]
    pub fn of(der: &[u8]) -> Self {
        Self(Sha256::digest(der).into())
    }

    /// The digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Fingerprint {
    /// Lower-case hex.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// A certificate the guest gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedCert {
    der: Vec<u8>,
    fingerprint: Fingerprint,
    kind: CertKind,
    subject_cn: Option<String>,
    not_after: i64,
    sources: Vec<StoreSource>,
}

impl SyncedCert {
    /// The certificate, DER.
    #[must_use]
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// The certificate as one PEM block (LF line ends).
    #[must_use]
    pub fn pem(&self) -> String {
        puddle_ca::certificate_pem(&self.der)
    }

    /// SHA-256 of the DER.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// Root or intermediate.
    #[must_use]
    pub fn kind(&self) -> CertKind {
        self.kind
    }

    /// The subject's common name, for display.
    #[must_use]
    pub fn subject_cn(&self) -> Option<&str> {
        self.subject_cn.as_deref()
    }

    /// `notAfter`, as seconds since the Unix epoch.
    #[must_use]
    pub fn not_after_unix(&self) -> i64 {
        self.not_after
    }

    /// Every store it was found in, sorted.
    #[must_use]
    pub fn sources(&self) -> &[StoreSource] {
        &self.sources
    }
}

/// A certificate from the stores that the guest doesn't get, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedCert {
    /// SHA-256 of the DER.
    pub fingerprint: Fingerprint,
    /// The subject's common name, when readable.
    pub subject_cn: Option<String>,
    /// Every store it was found in, sorted.
    pub sources: Vec<StoreSource>,
    /// Why it is not synced.
    pub reason: SkipReason,
}

/// The host's admin- and user-added roots (and the intermediates they issued) that the guest
/// trusts, plus what was left out, for the network health page.
///
/// Selection: every certificate from a `Root` store; from a `CA` store only
/// those issued by a synced certificate; never one whose certificate or public key is in a
/// `Disallowed` store; never an expired one. Each certificate once, however many stores hold
/// it, sorted by fingerprint so the same stores give byte-identical guest files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CorporateRoots {
    synced: Vec<SyncedCert>,
    skipped: Vec<SkippedCert>,
    unreadable: Vec<UnreadableStore>,
}

/// One distinct certificate of the snapshot with every store it was in.
struct Candidate<'a> {
    der: &'a [u8],
    sources: BTreeSet<StoreSource>,
}

impl CorporateRoots {
    /// Selects from `snapshot` as of `now`.
    #[must_use]
    pub fn select(snapshot: &StoreSnapshot, now: SystemTime) -> Self {
        let now = unix_seconds(now);
        let mut distrusted_certs = BTreeSet::new();
        let mut distrusted_keys = BTreeSet::new();
        let mut candidates: BTreeMap<Fingerprint, Candidate<'_>> = BTreeMap::new();
        for cert in snapshot.certs() {
            let fingerprint = Fingerprint::of(&cert.der);
            if cert.source.store == StoreName::Disallowed {
                distrusted_certs.insert(fingerprint);
                if let Ok(fields) = der::parse(&cert.der) {
                    distrusted_keys.insert(fields.spki.to_vec());
                }
                continue;
            }
            candidates
                .entry(fingerprint)
                .or_insert_with(|| Candidate {
                    der: &cert.der,
                    sources: BTreeSet::new(),
                })
                .sources
                .insert(cert.source);
        }

        let mut out = Self {
            unreadable: snapshot.unreadable().to_vec(),
            ..Self::default()
        };
        // Intermediates wait until their issuer is known to be synced.
        let mut pending = Vec::new();
        for (fingerprint, candidate) in candidates {
            let sources: Vec<StoreSource> = candidate.sources.into_iter().collect();
            let skip = |reason, subject_cn| SkippedCert {
                fingerprint,
                subject_cn,
                sources: sources.clone(),
                reason,
            };
            let Ok(fields) = der::parse(candidate.der) else {
                out.skipped.push(skip(SkipReason::Unreadable, None));
                continue;
            };
            let cn = fields.subject_cn.clone();
            if distrusted_certs.contains(&fingerprint) || distrusted_keys.contains(fields.spki) {
                out.skipped.push(skip(SkipReason::Disallowed, cn));
            } else if fields.not_after < now {
                out.skipped.push(skip(SkipReason::Expired, cn));
            } else {
                let is_root = sources.iter().any(|s| s.store == StoreName::Root);
                let cert = SyncedCert {
                    der: candidate.der.to_vec(),
                    fingerprint,
                    kind: if is_root {
                        CertKind::Root
                    } else {
                        CertKind::Intermediate
                    },
                    subject_cn: cn,
                    not_after: fields.not_after,
                    sources,
                };
                if is_root {
                    out.synced.push(cert);
                } else {
                    pending.push((cert, fields.issuer.to_vec()));
                }
            }
        }

        // Add intermediates issued by a synced certificate until nothing changes (a chain of
        // corporate intermediates comes in one at a time).
        let mut subjects: BTreeSet<Vec<u8>> = out
            .synced
            .iter()
            .filter_map(|c| der::parse(&c.der).ok().map(|f| f.subject.to_vec()))
            .collect();
        loop {
            let (issued, rest): (Vec<_>, Vec<_>) = pending
                .into_iter()
                .partition(|(_, issuer)| subjects.contains(issuer));
            pending = rest;
            if issued.is_empty() {
                break;
            }
            for (cert, _) in issued {
                if let Ok(f) = der::parse(&cert.der) {
                    subjects.insert(f.subject.to_vec());
                }
                out.synced.push(cert);
            }
        }
        for (cert, _) in pending {
            out.skipped.push(SkippedCert {
                fingerprint: cert.fingerprint,
                subject_cn: cert.subject_cn,
                sources: cert.sources,
                reason: SkipReason::NotIssuedBySyncedCert,
            });
        }
        out.synced.sort_by_key(|c| c.fingerprint);
        out.skipped.sort_by_key(|c| c.fingerprint);
        out
    }

    /// The certificates the guest gets, sorted by fingerprint.
    #[must_use]
    pub fn certificates(&self) -> &[SyncedCert] {
        &self.synced
    }

    /// The plain-HTTP URLs the synced certificates name as CRL distribution points or in their
    /// authority information access (the issuer's certificate, the OCSP responder), sorted and
    /// without repeats: what a client of the corporate CA fetches to check a chain, and the
    /// destinations the proxy lets through by themselves. Read at sync, so it follows the
    /// stores the guest's roots came from.
    #[must_use]
    pub fn distribution_urls(&self) -> Vec<String> {
        let mut urls: Vec<String> = self
            .synced
            .iter()
            .flat_map(|cert| crate::pki::http_urls(&cert.der))
            .collect();
        urls.sort_unstable();
        urls.dedup();
        urls
    }

    /// What was left out and why, sorted by fingerprint.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedCert] {
        &self.skipped
    }

    /// Stores that could not be read (their certificates are missing above).
    #[must_use]
    pub fn unreadable_stores(&self) -> &[UnreadableStore] {
        &self.unreadable
    }

    /// Whether nothing is synced.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.synced.is_empty()
    }
}

fn unix_seconds(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_secs()).map_or(i64::MIN, |s| -s),
    }
}

#[cfg(test)]
mod tests;
