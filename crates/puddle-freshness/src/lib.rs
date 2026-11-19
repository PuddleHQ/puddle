// SPDX-License-Identifier: AGPL-3.0-or-later
//! The freshness file of the update list.
//!
//! A signed release list proves who published it, not that it is current: whoever can serve old
//! bytes (a mirror, a cache, a network attacker) can keep a client on a release with a known hole
//! (a rollback) or hide every new release (a freeze). The freshness file closes both. It is one
//! small signed statement that a scheduled job re-signs every day with a higher `sequence` and a
//! new `expires_at`, about seven days out. A client that has seen sequence N refuses anything
//! lower, and refuses any file whose `expires_at` has passed, so an attacker can replay a file for
//! at most its remaining lifetime.
//!
//! The file is JSON: `{"signed": "<statement JSON text>", "signature": "<base64 Ed25519>"}`. The
//! signature covers the exact bytes of the `signed` string, so there is no canonicalisation to
//! get wrong. [`verify`] is the client's whole check; [`refresh`] is what the scheduled job runs.
//!
//! This crate holds no key. The client passes the trusted public keys in (compiled into the
//! program), and the signing job passes the seed of the freshness key in from a repository
//! secret. [`PLACEHOLDER_SEED`] is a public test key that no client trusts.
#![forbid(unsafe_code)]

pub mod cli;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The only format version this code reads and writes.
pub const FORMAT_VERSION: u32 = 1;

/// How long a refreshed file stays valid: seven days.
pub const DEFAULT_VALIDITY_SECS: u64 = 7 * 24 * 60 * 60;

/// How far in the future `issued_at` may be before the client calls the file not yet valid (clock
/// skew between the signer and the client).
pub const CLOCK_SKEW_SECS: u64 = 5 * 60;

/// A public test seed. The scheduled job signs with it while no production key is configured;
/// no client lists its public key, so a file signed with it is never accepted anywhere.
pub const PLACEHOLDER_SEED: [u8; 32] = [0x50; 32];

/// Why a freshness file is refused (or could not be made).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FreshnessError {
    /// Not JSON of the expected shape, cut short, or a field of the wrong kind.
    #[error("the freshness file is malformed: {0}")]
    Malformed(String),
    /// No trusted key signed these bytes.
    #[error("the freshness file's signature is not from a trusted key")]
    BadSignature,
    /// A format version this code does not know.
    #[error("freshness format version {0} is not supported")]
    UnsupportedVersion(u32),
    /// The statement is about another project.
    #[error("the freshness file is for project {found:?}, not {expected:?}")]
    WrongProject {
        /// The project the client updates.
        expected: String,
        /// The project the file names.
        found: String,
    },
    /// `expires_at` is not after `issued_at`.
    #[error("the freshness file expires ({expires_at}) before it was issued ({issued_at})")]
    EmptyWindow {
        /// Unix seconds.
        issued_at: u64,
        /// Unix seconds.
        expires_at: u64,
    },
    /// The file claims to stay valid longer than the client allows (a leaked key could otherwise
    /// sign a file that never expires).
    #[error("the freshness file is valid for {secs} s, the client allows at most {max} s")]
    ValidityTooLong {
        /// The file's window.
        secs: u64,
        /// The client's cap.
        max: u64,
    },
    /// Issued in the future, beyond the allowed clock skew.
    #[error("the freshness file is issued at {issued_at}, after now ({now})")]
    NotYetValid {
        /// Unix seconds.
        issued_at: u64,
        /// The client's clock.
        now: u64,
    },
    /// Past `expires_at`: the list may be stale.
    #[error("the freshness file expired at {expires_at} (now {now})")]
    Expired {
        /// Unix seconds.
        expires_at: u64,
        /// The client's clock.
        now: u64,
    },
    /// A lower sequence than one the client already accepted: a replayed old file.
    #[error("freshness sequence {got} is below the {seen} this client has already accepted")]
    SequenceRollback {
        /// The highest accepted so far.
        seen: u64,
        /// The offered one.
        got: u64,
    },
    /// The same sequence as one already accepted, with different content.
    #[error("freshness sequence {0} was already accepted with different content")]
    SequenceConflict(u64),
    /// A sequence above the ceiling the client allows (a leaked key could otherwise jump to the
    /// maximum and block every later file).
    #[error("freshness sequence {got} is above the ceiling {ceiling}")]
    SequenceCeiling {
        /// The client's ceiling.
        ceiling: u64,
        /// The offered one.
        got: u64,
    },
    /// The previous file could not be advanced (its sequence is the maximum).
    #[error("the sequence cannot go higher than {0}")]
    SequenceOverflow(u64),
}

/// What the signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Statement {
    /// [`FORMAT_VERSION`].
    pub version: u32,
    /// The project whose update list this vouches for.
    pub project: String,
    /// Raised by one at every refresh; the client never accepts a lower one.
    pub sequence: u64,
    /// When it was signed, in Unix seconds.
    pub issued_at: u64,
    /// When the client stops trusting it, in Unix seconds.
    pub expires_at: u64,
}

/// The file as stored.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    signed: String,
    signature: String,
}

/// The client's rules.
#[derive(Debug, Clone, Copy)]
pub struct Policy<'a> {
    /// The project the client updates.
    pub project: &'a str,
    /// The longest `expires_at - issued_at` it accepts.
    pub max_validity_secs: u64,
    /// The highest sequence it accepts.
    pub sequence_ceiling: u64,
}

/// A file whose signature checked out, not yet checked for freshness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signed(Statement);

impl Signed {
    /// Parses `bytes` and checks the signature against `keys`.
    ///
    /// # Errors
    ///
    /// [`FreshnessError::Malformed`] for bytes that are not a whole freshness file,
    /// [`FreshnessError::BadSignature`] when no key in `keys` signed it,
    /// [`FreshnessError::UnsupportedVersion`] for another format version.
    pub fn open(bytes: &[u8], keys: &[VerifyingKey]) -> Result<Self, FreshnessError> {
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|e| FreshnessError::Malformed(e.to_string()))?;
        let raw = STANDARD
            .decode(&envelope.signature)
            .map_err(|e| FreshnessError::Malformed(format!("signature: {e}")))?;
        let signature = Signature::from_slice(&raw)
            .map_err(|e| FreshnessError::Malformed(format!("signature: {e}")))?;
        if !keys.iter().any(|key| {
            key.verify_strict(envelope.signed.as_bytes(), &signature)
                .is_ok()
        }) {
            return Err(FreshnessError::BadSignature);
        }
        let statement: Statement = serde_json::from_str(&envelope.signed)
            .map_err(|e| FreshnessError::Malformed(format!("statement: {e}")))?;
        if statement.version != FORMAT_VERSION {
            return Err(FreshnessError::UnsupportedVersion(statement.version));
        }
        Ok(Self(statement))
    }

    /// The signed statement.
    #[must_use]
    pub fn statement(&self) -> &Statement {
        &self.0
    }

    /// Checks the statement against the client's rules, its clock and the newest statement it
    /// accepted before (`last_seen`; `None` on first use).
    ///
    /// # Errors
    ///
    /// The first rule that fails: project, window, validity cap, not yet valid, expired,
    /// sequence ceiling, then rollback or conflict against `last_seen`.
    pub fn check(
        &self,
        policy: &Policy<'_>,
        now: u64,
        last_seen: Option<&Statement>,
    ) -> Result<&Statement, FreshnessError> {
        let s = &self.0;
        if s.project != policy.project {
            return Err(FreshnessError::WrongProject {
                expected: policy.project.to_owned(),
                found: s.project.clone(),
            });
        }
        if s.expires_at <= s.issued_at {
            return Err(FreshnessError::EmptyWindow {
                issued_at: s.issued_at,
                expires_at: s.expires_at,
            });
        }
        let secs = s.expires_at - s.issued_at;
        if secs > policy.max_validity_secs {
            return Err(FreshnessError::ValidityTooLong {
                secs,
                max: policy.max_validity_secs,
            });
        }
        if s.issued_at > now.saturating_add(CLOCK_SKEW_SECS) {
            return Err(FreshnessError::NotYetValid {
                issued_at: s.issued_at,
                now,
            });
        }
        if now >= s.expires_at {
            return Err(FreshnessError::Expired {
                expires_at: s.expires_at,
                now,
            });
        }
        if s.sequence > policy.sequence_ceiling {
            return Err(FreshnessError::SequenceCeiling {
                ceiling: policy.sequence_ceiling,
                got: s.sequence,
            });
        }
        if let Some(seen) = last_seen {
            if s.sequence < seen.sequence {
                return Err(FreshnessError::SequenceRollback {
                    seen: seen.sequence,
                    got: s.sequence,
                });
            }
            if s.sequence == seen.sequence && s != seen {
                return Err(FreshnessError::SequenceConflict(s.sequence));
            }
        }
        Ok(s)
    }
}

/// The client's whole check: signature, then freshness. The caller stores the returned statement
/// as the new `last_seen` and keeps the file's bytes only until the next successful check.
///
/// # Errors
///
/// Any [`FreshnessError`] of [`Signed::open`] and [`Signed::check`].
pub fn verify(
    bytes: &[u8],
    keys: &[VerifyingKey],
    policy: &Policy<'_>,
    now: u64,
    last_seen: Option<&Statement>,
) -> Result<Statement, FreshnessError> {
    let signed = Signed::open(bytes, keys)?;
    signed.check(policy, now, last_seen).cloned()
}

/// Signs `statement` with the key whose seed is `seed` and returns the file's bytes.
#[must_use]
pub fn sign(seed: &[u8; 32], statement: &Statement) -> Vec<u8> {
    // A statement of strings and integers always serialises.
    let signed = serde_json::to_string(statement).unwrap_or_default();
    let signature = SigningKey::from_bytes(seed).sign(signed.as_bytes());
    let envelope = Envelope {
        signed,
        signature: STANDARD.encode(signature.to_bytes()),
    };
    let mut out = serde_json::to_vec_pretty(&envelope).unwrap_or_default();
    out.push(b'\n');
    out
}

/// A freshly signed file and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refreshed {
    /// The bytes of the file to publish.
    pub bytes: Vec<u8>,
    /// The statement it carries.
    pub statement: Statement,
}

/// What the scheduled job runs: the next file after `previous` (the file now published, if any),
/// signed with `seed`, valid for `validity_secs` from `now`.
///
/// The previous file must carry a valid signature from the same key, but it may have expired: the
/// job has to recover after an outage. A previous file that is unreadable or signed by another
/// key is an error, never a fresh start, because a file may only be replaced by one its own key
/// signed. With no previous file the sequence is the count of minutes since the epoch at `now`:
/// each run adds one and runs come at most once a minute, so a start over after the file was lost
/// still lands above every sequence a client may have seen, never a rollback.
///
/// # Errors
///
/// [`FreshnessError::Malformed`] or [`FreshnessError::BadSignature`] for `previous`,
/// [`FreshnessError::SequenceOverflow`] when its sequence cannot be raised.
pub fn refresh(
    previous: Option<&[u8]>,
    seed: &[u8; 32],
    project: &str,
    now: u64,
    validity_secs: u64,
) -> Result<Refreshed, FreshnessError> {
    let sequence = match previous {
        // The minute count since the epoch: a daily job gives out at most one sequence a minute,
        // so a start over (the published file lost) is above every sequence given out before.
        None => now / 60,
        Some(bytes) => {
            let key = SigningKey::from_bytes(seed).verifying_key();
            let before = Signed::open(bytes, &[key])?;
            let seq = before.statement().sequence;
            seq.checked_add(1)
                .ok_or(FreshnessError::SequenceOverflow(seq))?
        }
    };
    let statement = Statement {
        version: FORMAT_VERSION,
        project: project.to_owned(),
        sequence,
        issued_at: now,
        expires_at: now.saturating_add(validity_secs),
    };
    Ok(Refreshed {
        bytes: sign(seed, &statement),
        statement,
    })
}

/// The public half of the key with this seed.
#[must_use]
pub fn public_key(seed: &[u8; 32]) -> VerifyingKey {
    SigningKey::from_bytes(seed).verifying_key()
}

#[cfg(test)]
mod tests;
