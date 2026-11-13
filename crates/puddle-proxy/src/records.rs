// SPDX-License-Identifier: GPL-3.0-or-later
//! `SRV`, `TXT` and `MX` lookups for names the rules allow (the guest's stub DNS forwards them;
//! `mongodb+srv` and Kerberos need them). Address lookups go through [`crate::Resolver`].
//!
//! The lookups use the host's own DNS configuration, like the address resolver does, so a name
//! the host can't resolve has no records here either; the answer is then `NODATA` or `NXDOMAIN`
//! and the guest's client reports it as it would on any network that lacks the record.

use std::time::Duration;

use hickory_resolver::TokioResolver;
use hickory_resolver::proto::rr::{RData, RecordType as DnsType};
use puddle_agent_proto::resolve::{MAX_RECORDS, Record, RecordType};

use crate::destination::BoxFuture;

/// Shortest and longest time an answer may be cached by the guest.
const TTL_RANGE: (u32, u32) = (5, 300);

/// Longest TXT string kept, and most TXT bytes in one answer.
const MAX_TXT_BYTES: usize = 8 * 1024;

/// The records found for a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Records {
    /// The records, at most [`MAX_RECORDS`].
    pub records: Vec<Record>,
    /// How long the answer may be cached.
    pub ttl: Duration,
}

/// Why a record lookup has no records.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RecordError {
    /// The name doesn't exist.
    #[error("no such name")]
    NoSuchName,
    /// The name exists, with no record of this type.
    #[error("no records of this type")]
    NoData,
    /// The lookup failed (no server, timeout, malformed answer).
    #[error("lookup failed: {0}")]
    Failed(String),
}

/// Looks up `SRV`, `TXT` and `MX` records of an allowed name. Tests fake it;
/// [`SystemRecords`] asks the host's DNS servers.
pub trait RecordResolver: Send + Sync {
    /// The records of type `rtype` for `fqdn`: lower case, no trailing dot, labels of letters,
    /// digits, `-` and `_` only (the proxy has checked it).
    fn records<'a>(
        &'a self,
        fqdn: &'a str,
        rtype: RecordType,
    ) -> BoxFuture<'a, Result<Records, RecordError>>;
}

/// The DNS type a record type is asked for with; `None` for the types this resolver does not serve
/// (address lookups go through [`crate::Resolver`]).
fn dns_type(rtype: RecordType) -> Option<DnsType> {
    match rtype {
        RecordType::Srv => Some(DnsType::SRV),
        RecordType::Txt => Some(DnsType::TXT),
        RecordType::Mx => Some(DnsType::MX),
        RecordType::A => None,
    }
}

/// The host's DNS configuration (`/etc/resolv.conf`, the Windows adapter settings), read at each
/// lookup so a network change is picked up.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemRecords;

impl RecordResolver for SystemRecords {
    fn records<'a>(
        &'a self,
        fqdn: &'a str,
        rtype: RecordType,
    ) -> BoxFuture<'a, Result<Records, RecordError>> {
        Box::pin(async move {
            let Some(dns_type) = dns_type(rtype) else {
                return Err(RecordError::NoData);
            };
            let resolver: TokioResolver = TokioResolver::builder_tokio()
                .and_then(hickory_resolver::ResolverBuilder::build)
                .map_err(|err| RecordError::Failed(err.to_string()))?;
            // The trailing dot makes it a single query: no search-domain guesses.
            let lookup = resolver
                .lookup(format!("{fqdn}."), dns_type)
                .await
                .map_err(|err| {
                    if err.is_nx_domain() {
                        RecordError::NoSuchName
                    } else if err.is_no_records_found() {
                        RecordError::NoData
                    } else {
                        RecordError::Failed(err.to_string())
                    }
                })?;
            let (records, ttl) = collect(fqdn, lookup.answers().iter().map(|a| (&a.data, a.ttl)));
            if records.is_empty() {
                return Err(RecordError::NoData);
            }
            let ttl = ttl.clamp(TTL_RANGE.0, TTL_RANGE.1);
            Ok(Records {
                records,
                ttl: Duration::from_secs(u64::from(ttl)),
            })
        })
    }
}

/// How many records [`collect`] left out, only to say so in a log line. A type of its own so that a
/// mutation exclusion can name these counters without also covering the byte counter beside them.
#[derive(Default)]
struct LeftOut {
    skipped: usize,
    truncated: usize,
}

impl LeftOut {
    fn bump_skipped(&mut self) {
        self.skipped += 1;
    }

    fn bump_truncated(&mut self) {
        self.truncated += 1;
    }

    fn any(&self) -> bool {
        self.skipped + self.truncated > 0
    }
}

/// The records of `answers` the stub can carry, and the smallest time to live among them. What is
/// left out (another type, a target that is not a plain host name, TXT past its byte cap, more
/// than [`MAX_RECORDS`]) is said in the log: the guest sees a shorter answer.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "hickory's RData has dozens of record types (and a hidden unknown one); only SRV, MX and TXT are carried and every other type is left out"
)]
fn collect<'a>(fqdn: &str, answers: impl Iterator<Item = (&'a RData, u32)>) -> (Vec<Record>, u32) {
    let mut records = Vec::new();
    let mut ttl = TTL_RANGE.1;
    let mut txt_bytes = 0;
    let mut left_out = LeftOut::default();
    for (data, answer_ttl) in answers {
        if records.len() >= MAX_RECORDS {
            left_out.bump_truncated();
            continue;
        }
        let record = match data {
            RData::SRV(srv) => target_name(&srv.target.to_ascii()).map(|target| Record::Srv {
                priority: srv.priority,
                weight: srv.weight,
                port: srv.port,
                target,
            }),
            RData::MX(mx) => target_name(&mx.exchange.to_ascii()).map(|exchange| Record::Mx {
                preference: mx.preference,
                exchange,
            }),
            RData::TXT(txt) => {
                let strings: Vec<String> = txt
                    .txt_data
                    .iter()
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
                txt_bytes += strings.iter().map(String::len).sum::<usize>();
                (txt_bytes <= MAX_TXT_BYTES).then_some(Record::Txt { strings })
            }
            _ => None,
        };
        if let Some(record) = record {
            ttl = ttl.min(answer_ttl);
            records.push(record);
        } else {
            left_out.bump_skipped();
        }
    }
    if left_out.any() {
        tracing::warn!(host = %fqdn, skipped = left_out.skipped, truncated = left_out.truncated, kept = records.len(), "some records of the answer were left out: an unsupported or oversized record, a target that is not a plain host name, or more than an answer may carry");
    }
    (records, ttl)
}

/// A record's target as the protocol carries it: lower case, no trailing dot. `None` for the root
/// (an SRV target of `.` means "no service") and for anything that isn't a plain host name.
fn target_name(ascii: &str) -> Option<String> {
    let name = ascii
        .strip_suffix('.')
        .unwrap_or(ascii)
        .to_ascii_lowercase();
    valid_fqdn(&name).then_some(name)
}

/// Whether `name` is a syntactically plain name: labels of 1-63 letters, digits, `-` or `_`,
/// at most 253 characters in all.
pub(crate) fn valid_fqdn(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= puddle_agent_proto::resolve::MAX_NAME
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        })
}

#[cfg(test)]
mod tests;
