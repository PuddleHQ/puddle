// SPDX-License-Identifier: AGPL-3.0-or-later
//! The control stream: one yamux stream per session on which the agent reports to the host.
//!
//! The agent opens it like any other stream and writes [`PREAMBLE`], then one JSON
//! [`AgentMessage`] per line (`\n`-terminated, at most [`MAX_LINE`] bytes including the newline).
//! Data flows guest → host only. The first message is [`AgentMessage::Hello`].
//!
//! Host → guest messages use the same line format on a host-opened `host-control` stream
//! ([`crate::host_control`]).
//!
//! ```
//! use puddle_agent_proto::AgentMessage;
//! let line = AgentMessage::oom_kill(Some(42), Some("node".into())).to_line().unwrap();
//! assert_eq!(line, b"{\"type\":\"oom_kill\",\"pid\":42,\"process\":\"node\"}\n");
//! assert_eq!(AgentMessage::from_line(&line).unwrap(), AgentMessage::oom_kill(Some(42), Some("node".into())));
//! ```

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

/// Bytes that open a control stream ([`crate::kind::StreamKind::Control`] v1).
pub const PREAMBLE: &[u8] = crate::kind::StreamKind::Control.preamble();

/// The control protocol version, sent in [`AgentMessage::Hello`]. Bump it on a change the other
/// side must know about; adding a message type is not one (unknown types are ignored).
pub const PROTOCOL_VERSION: u32 = 1;

/// Longest control line the host accepts, newline included. A longer line ends the stream.
pub const MAX_LINE: usize = 4096;

/// Most ports one [`AgentMessage::Ports`] carries: five digits and a comma each, so a full report
/// stays well inside [`MAX_LINE`].
pub const MAX_PORTS: usize = 512;

/// Most characters of an [`AgentMessage::EditorStatus`] reason the agent sends.
pub const MAX_EDITOR_PROBLEM_CHARS: usize = 200;

/// One message from the agent to the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentMessage {
    /// Sent first on every control stream.
    Hello {
        /// The agent's version (Puddle's version at build time).
        agent_version: String,
        /// [`PROTOCOL_VERSION`] of the agent.
        protocol: u32,
    },
    /// The guest kernel's OOM killer ended a process.
    ///
    /// `pid` and `process` are missing when the agent saw the kill counter in `/proc/vmstat` go up
    /// but no matching `Killed process` line in the kernel log.
    OomKill {
        /// The killed process's id in the guest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        /// The killed process's name (`comm`) as the kernel printed it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        process: Option<String>,
    },
    /// The capture rules in the guest were deleted or changed by something other than the agent;
    /// the agent put them back.
    CaptureTampered {
        /// `true` when the whole table was gone, `false` when it was edited.
        table_missing: bool,
    },
    /// The agent could not install the capture rules (no netfilter in the kernel, no permission):
    /// until it can, tools in the workspace that ignore the proxy settings see an offline network.
    /// Sent once per outage; the reason is the agent's own error text.
    CaptureUnavailable {
        /// What the kernel or the agent said.
        reason: String,
    },
    /// The capture rules are installed after an outage the agent told with
    /// [`AgentMessage::CaptureUnavailable`]. An agent from before this message never sends it, and
    /// a host from before it ignores it.
    CaptureReady,
    /// Datagrams the capture rules refused: UDP that is not DNS fails at once in the guest, and
    /// the agent says where it was going. The address and port are the guest's claim, as is the
    /// count; the host cleans them.
    UdpRefused {
        /// The destination address as text.
        address: String,
        /// The destination port.
        port: u16,
        /// Datagrams refused since the last report for this destination.
        count: u32,
    },
    /// The image's entrypoint, which the boot hook started, exited. `tail` is the end of its
    /// output as the hook's wrapper wrote it. Both are the guest's claim; the host cleans them.
    EntrypointExited {
        /// The exit status as the shell gives it (a signal is 128 plus its number).
        status: u8,
        /// The last lines of its output, joined into one line.
        #[serde(default)]
        tail: String,
    },
    /// The stub DNS turned something away or could not carry it. Sent at once for the first one of
    /// a kind, then at most once every ten seconds per kind with the number since the last report.
    StubDns {
        /// What happened.
        kind: StubDnsTrouble,
        /// How often since the last report of this kind.
        count: u32,
    },
    /// Every TCP port a process in the guest listens on right now, sorted, without the ports the
    /// agent itself serves. Sent whenever the set changes (and once at start), and always the whole
    /// set: the latest report replaces the one before. At most [`MAX_PORTS`] ports; the claim is the
    /// guest's own, and the host clamps it before use.
    Ports {
        /// The ports, ascending.
        listening: Vec<u16>,
    },
    /// How the browser editor's server is doing in the guest, one problem per `kind`. An
    /// [`EditorProblemKind::Install`] problem is sent each time a download of the server is
    /// refused (a wrong checksum, an archive that does not unpack) or has failed on the network
    /// for several tries, and unset once a server launches after it. An
    /// [`EditorProblemKind::Update`] one is sent for a newer build that was refused, one that
    /// started badly and was rolled back, or looks that keep failing, and unset by the next look
    /// that works. An [`EditorProblemKind::SessionUser`] one is sent once per start of the server
    /// whose session user the guest does not list, and unset when a start finds it. The latest
    /// message of a kind replaces the one before; the text is the agent's
    /// own error, at most [`MAX_EDITOR_PROBLEM_CHARS`] characters, and the host cleans it.
    EditorStatus {
        /// Which problem this is about; an agent from before there were two sends none, which
        /// reads as an install problem.
        #[serde(default)]
        kind: EditorProblemKind,
        /// What is wrong; `None` when the problem of this `kind` is over.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// A message type this side doesn't know (from a newer agent). Ignored.
    #[serde(other)]
    Unknown,
}

/// The things that can be wrong with the browser editor's server ([`AgentMessage::EditorStatus`]).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EditorProblemKind {
    /// The server cannot be installed, so the editor cannot open.
    #[default]
    Install,
    /// The editor works on the build it has; a newer one could not be taken.
    Update,
    /// The editor works, but its server runs as root because the session user is missing in the
    /// guest.
    SessionUser,
    /// A kind this side doesn't know (from a newer agent).
    #[serde(other)]
    Unknown,
}

/// What the stub DNS reports in [`AgentMessage::StubDns`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StubDnsTrouble {
    /// A lookup on the host failed; the client got `SERVFAIL`.
    LookupFailed,
    /// A lookup on the host did not answer in time; the client got `SERVFAIL`.
    LookupTimedOut,
    /// No lookup slot came free in time; the query got no answer and the client asks again.
    NoLookupSlot,
    /// The stand-in address range had no address left; the client got `SERVFAIL`.
    NoStandInAddress,
    /// Records the stub cannot carry were left out of an answer.
    RecordsLeftOut,
    /// UDP queries were dropped because too many were open at once; the client asks again.
    UdpDropped,
    /// TCP connections were closed because too many were open at once; the client asks again.
    TcpRefused,
    /// A kind this side doesn't know (from a newer agent).
    #[serde(other)]
    Unknown,
}

impl StubDnsTrouble {
    /// Every kind an agent sends.
    pub const ALL: [Self; 7] = [
        Self::LookupFailed,
        Self::LookupTimedOut,
        Self::NoLookupSlot,
        Self::NoStandInAddress,
        Self::RecordsLeftOut,
        Self::UdpDropped,
        Self::TcpRefused,
    ];
}

impl AgentMessage {
    /// A [`AgentMessage::Hello`] for this build.
    #[must_use]
    pub fn hello() -> Self {
        Self::Hello {
            agent_version: puddle_types::VERSION.to_owned(),
            protocol: PROTOCOL_VERSION,
        }
    }

    /// An [`AgentMessage::OomKill`].
    #[must_use]
    pub fn oom_kill(pid: Option<u32>, process: Option<String>) -> Self {
        Self::OomKill { pid, process }
    }

    /// An [`AgentMessage::CaptureTampered`].
    #[must_use]
    pub fn capture_tampered(table_missing: bool) -> Self {
        Self::CaptureTampered { table_missing }
    }

    /// An [`AgentMessage::CaptureUnavailable`].
    #[must_use]
    pub fn capture_unavailable(reason: String) -> Self {
        Self::CaptureUnavailable { reason }
    }

    /// An [`AgentMessage::CaptureReady`].
    #[must_use]
    pub fn capture_ready() -> Self {
        Self::CaptureReady
    }

    /// An [`AgentMessage::UdpRefused`].
    #[must_use]
    pub fn udp_refused(address: String, port: u16, count: u32) -> Self {
        Self::UdpRefused {
            address,
            port,
            count,
        }
    }

    /// An [`AgentMessage::EntrypointExited`].
    #[must_use]
    pub fn entrypoint_exited(status: u8, tail: String) -> Self {
        Self::EntrypointExited { status, tail }
    }

    /// An [`AgentMessage::StubDns`].
    #[must_use]
    pub fn stub_dns(kind: StubDnsTrouble, count: u32) -> Self {
        Self::StubDns { kind, count }
    }

    /// An [`AgentMessage::EditorStatus`]; a `problem` longer than [`MAX_EDITOR_PROBLEM_CHARS`]
    /// characters is cut.
    #[must_use]
    pub fn editor_status(kind: EditorProblemKind, problem: Option<&str>) -> Self {
        Self::EditorStatus {
            kind,
            problem: problem.map(|p| p.chars().take(MAX_EDITOR_PROBLEM_CHARS).collect()),
        }
    }

    /// An [`AgentMessage::Ports`] for `listening`: sorted, without duplicates or port 0, cut to
    /// [`MAX_PORTS`].
    #[must_use]
    pub fn ports(listening: impl IntoIterator<Item = u16>) -> Self {
        let mut listening: Vec<u16> = listening.into_iter().filter(|p| *p != 0).collect();
        listening.sort_unstable();
        listening.dedup();
        listening.truncate(MAX_PORTS);
        Self::Ports { listening }
    }

    /// The message as one control line, newline included.
    ///
    /// # Errors
    ///
    /// [`LineError::TooLong`] if it doesn't fit in [`MAX_LINE`] (a process name can't make it
    /// that long; the check keeps the agent within what the host accepts).
    pub fn to_line(&self) -> Result<Vec<u8>, LineError> {
        let mut line = serde_json::to_vec(self).map_err(|e| LineError::Json(e.to_string()))?;
        line.push(b'\n');
        if line.len() > MAX_LINE {
            return Err(LineError::TooLong);
        }
        Ok(line)
    }

    /// Parses one control line (with or without its newline).
    ///
    /// # Errors
    ///
    /// [`LineError::Json`] if the line isn't a JSON object with a string `type`.
    pub fn from_line(line: &[u8]) -> Result<Self, LineError> {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        serde_json::from_slice(line).map_err(|e| LineError::Json(e.to_string()))
    }
}

/// A control line that can't be written or read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LineError {
    /// Longer than [`MAX_LINE`].
    #[error("control line longer than {MAX_LINE} bytes")]
    TooLong,
    /// Not a valid message.
    #[error("invalid control message: {0}")]
    Json(String),
}

/// Reads one `\n`-terminated line of at most `max` bytes (newline included) into `line`, which is
/// cleared first. Returns `Ok(false)` at a clean end of stream with no partial line.
///
/// # Errors
///
/// The reader's I/O errors; [`io::ErrorKind::InvalidData`] when the line runs past `max` bytes;
/// [`io::ErrorKind::UnexpectedEof`] when the stream ends inside a line.
pub async fn read_line<R>(reader: &mut R, line: &mut Vec<u8>, max: usize) -> io::Result<bool>
where
    R: AsyncBufRead + Unpin + ?Sized,
{
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(false)
            } else {
                Err(io::ErrorKind::UnexpectedEof.into())
            };
        }
        let (take, done) = match available.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (available.len(), false),
        };
        if line.len() + take > max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                LineError::TooLong,
            ));
        }
        line.extend_from_slice(available.get(..take).unwrap_or_default());
        reader.consume(take);
        if done {
            return Ok(true);
        }
    }
}

#[cfg(test)]
mod tests;
