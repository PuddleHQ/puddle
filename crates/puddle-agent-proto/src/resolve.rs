// SPDX-License-Identifier: GPL-3.0-or-later
//! The `resolve` stream: the guest's stub DNS asks the host about one name.
//!
//! The agent opens a stream per lookup and writes [`PREAMBLE`], then one JSON [`ResolveQuery`]
//! line. The host answers with one JSON [`ResolveAnswer`] line and closes the stream. Lines are
//! `\n`-terminated and bounded ([`MAX_QUERY_LINE`], [`MAX_ANSWER_LINE`]).
//!
//! The host decides with the workspace's rules and never writes a pending row for a lookup:
//!
//! - a name the rules allow is looked up on the host, and the answer says whether the guest should
//!   get a stand-in address for it ([`ResolveAnswer::StandIn`]) or `NXDOMAIN`;
//! - a name that isn't allowed gets a stand-in **without any lookup**, so DNS is no channel out
//!   and the connect that follows produces the deny or pending row;
//! - `SRV`, `TXT` and `MX` records are returned for allowed names only.
//!
//! ```
//! use puddle_agent_proto::resolve::{RecordType, ResolveQuery};
//! let q = ResolveQuery::new("example.com", RecordType::A);
//! assert_eq!(q.to_line().unwrap(), b"{\"name\":\"example.com\",\"type\":\"A\"}\n");
//! ```

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::control::read_line;

/// Bytes that open a resolve stream ([`crate::kind::StreamKind::Resolve`] v1).
pub const PREAMBLE: &[u8] = crate::kind::StreamKind::Resolve.preamble();

/// Longest query line the host reads, newline included.
pub const MAX_QUERY_LINE: usize = 512;

/// Longest answer line the agent reads, newline included.
pub const MAX_ANSWER_LINE: usize = 16 * 1024;

/// Most records one answer carries; the host cuts a longer list.
pub const MAX_RECORDS: usize = 16;

/// Longest host name the protocol carries (the DNS limit).
pub const MAX_NAME: usize = 253;

/// The record types the stub forwards. Address queries (`A`) are answered with stand-ins;
/// `AAAA` and every other type never leave the guest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RecordType {
    /// An address: the answer says whether the guest gets a stand-in.
    A,
    /// Service records (`mongodb+srv`, Kerberos).
    Srv,
    /// Text records.
    Txt,
    /// Mail exchangers.
    Mx,
}

/// One lookup, from the agent to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveQuery {
    /// The name asked for, lower case, without a trailing dot. Untrusted: the host normalises it.
    pub name: String,
    /// The record type.
    #[serde(rename = "type")]
    pub rtype: RecordType,
}

impl ResolveQuery {
    /// A query for `name`.
    #[must_use]
    pub fn new(name: impl Into<String>, rtype: RecordType) -> Self {
        Self {
            name: name.into(),
            rtype,
        }
    }

    /// The query as a line, newline included.
    ///
    /// # Errors
    ///
    /// [`ResolveError::TooLong`] when the line passes [`MAX_QUERY_LINE`].
    pub fn to_line(&self) -> Result<Vec<u8>, ResolveError> {
        let mut line =
            serde_json::to_vec(self).map_err(|err| ResolveError::Invalid(err.to_string()))?;
        line.push(b'\n');
        if line.len() > MAX_QUERY_LINE {
            return Err(ResolveError::TooLong);
        }
        Ok(line)
    }

    /// Parses a query line (with or without its newline).
    ///
    /// # Errors
    ///
    /// [`ResolveError::Invalid`] for anything that isn't a query.
    pub fn from_line(line: &[u8]) -> Result<Self, ResolveError> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        serde_json::from_slice(line).map_err(|err| ResolveError::Invalid(err.to_string()))
    }
}

/// Why the host says a name gets a stand-in. For logs and tests; the guest sees the same stand-in
/// for every reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StandInReason {
    /// Allowed, and the host resolved it.
    Resolves,
    /// Allowed, but the host can't resolve it and the company proxy will: the proxy decides.
    ViaUpstream,
    /// No rule allows it (denied or pending): never looked up. The connect decides.
    NotAllowed,
    /// The name is blocked by itself or by every address it resolves to: the connect says why.
    Blocked,
}

/// One record of an [`ResolveAnswer::Records`] answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Record {
    /// A service record.
    Srv {
        /// Priority.
        priority: u16,
        /// Weight.
        weight: u16,
        /// Port.
        port: u16,
        /// Target host name.
        target: String,
    },
    /// A text record.
    Txt {
        /// The character strings.
        strings: Vec<String>,
    },
    /// A mail exchanger.
    Mx {
        /// Preference.
        preference: u16,
        /// Exchanger host name.
        exchange: String,
    },
}

/// The host's answer to a [`ResolveQuery`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ResolveAnswer {
    /// Answer an `A` query with a stand-in address.
    StandIn {
        /// Why.
        why: StandInReason,
        /// Seconds the answer may be cached.
        ttl: u32,
    },
    /// The name doesn't exist (`NXDOMAIN`).
    NoSuchName {
        /// Seconds the answer may be cached.
        ttl: u32,
    },
    /// The name exists but has nothing of this type (`NODATA`).
    NoData {
        /// Seconds the answer may be cached.
        ttl: u32,
    },
    /// Records for an `SRV`, `TXT` or `MX` query.
    Records {
        /// The records, at most [`MAX_RECORDS`].
        records: Vec<Record>,
        /// Seconds the answer may be cached.
        ttl: u32,
    },
    /// The host couldn't answer now (policy unreadable, busy, timed out): the guest gets
    /// `SERVFAIL` and may retry.
    Unavailable,
    /// An answer this build doesn't know. Treated as [`ResolveAnswer::Unavailable`].
    #[serde(other)]
    Unknown,
}

impl ResolveAnswer {
    /// The answer as a line, newline included.
    ///
    /// # Errors
    ///
    /// [`ResolveError::TooLong`] when the line passes [`MAX_ANSWER_LINE`].
    pub fn to_line(&self) -> Result<Vec<u8>, ResolveError> {
        let mut line =
            serde_json::to_vec(self).map_err(|err| ResolveError::Invalid(err.to_string()))?;
        line.push(b'\n');
        if line.len() > MAX_ANSWER_LINE {
            return Err(ResolveError::TooLong);
        }
        Ok(line)
    }

    /// Parses an answer line (with or without its newline).
    ///
    /// # Errors
    ///
    /// [`ResolveError::Invalid`] for anything that isn't an answer.
    pub fn from_line(line: &[u8]) -> Result<Self, ResolveError> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        serde_json::from_slice(line).map_err(|err| ResolveError::Invalid(err.to_string()))
    }
}

/// A lookup that failed on the wire.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ResolveError {
    /// The line is longer than its limit.
    #[error("resolve line too long")]
    TooLong,
    /// The line isn't valid.
    #[error("invalid resolve message: {0}")]
    Invalid(String),
    /// The stream ended before an answer.
    #[error("resolve stream ended before an answer")]
    NoAnswer,
    /// The stream failed.
    #[error("resolve stream i/o: {0}")]
    Io(#[from] io::Error),
    /// No answer within the limit.
    #[error("no resolve answer within {0:?}")]
    Timeout(Duration),
}

/// Asks the host one question on a fresh stream (the agent side): writes the preamble and the
/// query, reads the answer line.
///
/// # Errors
///
/// [`ResolveError`] for a failed stream, a missing or invalid answer, or no answer within `limit`.
pub async fn ask<S>(
    stream: S,
    query: &ResolveQuery,
    limit: Duration,
) -> Result<ResolveAnswer, ResolveError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let line = query.to_line()?;
    let mut stream = BufReader::new(stream);
    let exchange = async {
        let mut request = PREAMBLE.to_vec();
        request.extend_from_slice(&line);
        stream.write_all(&request).await?;
        stream.flush().await?;
        let mut answer = Vec::with_capacity(128);
        if !read_line(&mut stream, &mut answer, MAX_ANSWER_LINE)
            .await
            .map_err(|err| {
                if err.kind() == io::ErrorKind::InvalidData {
                    ResolveError::TooLong
                } else {
                    ResolveError::Io(err)
                }
            })?
        {
            return Err(ResolveError::NoAnswer);
        }
        ResolveAnswer::from_line(&answer)
    };
    tokio::time::timeout(limit, exchange)
        .await
        .map_err(|_| ResolveError::Timeout(limit))?
}

/// Reads one query line (the host side, after the preamble).
///
/// # Errors
///
/// [`ResolveError`] for a stream that fails, ends or sends a line over [`MAX_QUERY_LINE`] or
/// anything that isn't a query.
pub async fn read_query<R>(reader: &mut R) -> Result<ResolveQuery, ResolveError>
where
    R: AsyncBufReadExt + AsyncReadExt + Unpin,
{
    let mut line = Vec::with_capacity(96);
    match read_line(reader, &mut line, MAX_QUERY_LINE).await {
        Ok(true) => ResolveQuery::from_line(&line),
        Ok(false) => Err(ResolveError::NoAnswer),
        Err(err) if err.kind() == io::ErrorKind::InvalidData => Err(ResolveError::TooLong),
        Err(err) => Err(ResolveError::Io(err)),
    }
}

#[cfg(test)]
mod tests;
