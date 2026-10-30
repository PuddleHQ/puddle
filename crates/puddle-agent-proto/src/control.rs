// SPDX-License-Identifier: GPL-3.0-or-later
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
    /// Whether the browser editor's server could be installed in the guest. Sent with `problem`
    /// set each time a download of the server is fetched and refused (a wrong checksum, an archive
    /// that does not unpack), and with it unset once a server launches after such a refusal. The
    /// latest message replaces the one before; the text is the agent's own error, at most
    /// [`MAX_EDITOR_PROBLEM_CHARS`] characters, and the host cleans it.
    EditorStatus {
        /// Why the server cannot be installed; `None` when it can.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        problem: Option<String>,
    },
    /// A message type this side doesn't know (from a newer agent). Ignored.
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
    pub fn editor_status(problem: Option<&str>) -> Self {
        Self::EditorStatus {
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
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn hello_carries_this_builds_version_and_protocol() {
        let line = AgentMessage::hello().to_line().unwrap();
        let text = String::from_utf8(line).unwrap();
        assert_eq!(
            text,
            format!(
                "{{\"type\":\"hello\",\"agent_version\":\"{}\",\"protocol\":1}}\n",
                puddle_types::VERSION
            )
        );
    }

    #[test]
    fn an_unnamed_oom_kill_has_no_pid_or_process_fields() {
        let line = AgentMessage::oom_kill(None, None).to_line().unwrap();
        assert_eq!(line, b"{\"type\":\"oom_kill\"}\n");
        assert_eq!(
            AgentMessage::from_line(&line).unwrap(),
            AgentMessage::oom_kill(None, None)
        );
    }

    #[test]
    fn a_ports_report_is_sorted_and_clean_and_always_fits_a_line() {
        assert_eq!(
            AgentMessage::ports([8080, 22, 8080, 0, 3000]),
            AgentMessage::Ports {
                listening: vec![22, 3000, 8080]
            }
        );
        let line = AgentMessage::ports([3000]).to_line().unwrap();
        assert_eq!(line, b"{\"type\":\"ports\",\"listening\":[3000]}\n");
        assert_eq!(
            AgentMessage::from_line(&line).unwrap(),
            AgentMessage::ports([3000])
        );
        let widest = AgentMessage::ports((60_000..).take(MAX_PORTS + 100));
        assert!(
            matches!(&widest, AgentMessage::Ports { listening } if listening.len() == MAX_PORTS)
        );
        assert!(widest.to_line().unwrap().len() < MAX_LINE);
    }

    #[test]
    fn unknown_types_and_fields_parse_as_unknown_or_are_ignored() {
        assert_eq!(
            AgentMessage::from_line(b"{\"type\":\"cpu_pressure\",\"x\":1}").unwrap(),
            AgentMessage::Unknown
        );
        assert_eq!(
            AgentMessage::from_line(b"{\"type\":\"oom_kill\",\"pid\":3,\"extra\":true}").unwrap(),
            AgentMessage::oom_kill(Some(3), None)
        );
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for bad in [
            &b""[..],
            b"{",
            b"[]",
            b"{\"pid\":1}",
            b"{\"type\":7}",
            b"{\"type\":\"oom_kill\",\"pid\":-1}",
            b"\xff\xfe",
        ] {
            assert!(
                matches!(AgentMessage::from_line(bad), Err(LineError::Json(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_message_too_long_for_the_host_is_refused_by_the_writer() {
        let long = AgentMessage::oom_kill(Some(1), Some("x".repeat(MAX_LINE)));
        assert_eq!(long.to_line(), Err(LineError::TooLong));
        assert!(LineError::TooLong.to_string().contains("4096"));
    }

    async fn lines(input: &[u8], max: usize) -> Vec<io::Result<Vec<u8>>> {
        let mut reader = tokio::io::BufReader::with_capacity(3, input);
        let mut out = Vec::new();
        let mut line = Vec::new();
        loop {
            match read_line(&mut reader, &mut line, max).await {
                Ok(true) => out.push(Ok(line.clone())),
                Ok(false) => return out,
                Err(e) => {
                    out.push(Err(e));
                    return out;
                }
            }
        }
    }

    #[tokio::test]
    async fn read_line_splits_on_newlines_across_buffer_refills() {
        let got = lines(b"ab\ncdefg\n\n", 16).await;
        let got: Vec<_> = got.into_iter().map(Result::unwrap).collect();
        assert_eq!(got, [&b"ab\n"[..], b"cdefg\n", b"\n"]);
    }

    #[tokio::test]
    async fn read_line_refuses_a_line_past_the_limit() {
        let got = lines(b"abc\nabcdefgh\n", 5).await;
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].as_ref().unwrap(), b"abc\n");
        assert_eq!(
            got[1].as_ref().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Exactly at the limit is fine.
        assert_eq!(lines(b"abcd\n", 5).await[0].as_ref().unwrap(), b"abcd\n");
    }

    #[tokio::test]
    async fn read_line_reports_a_stream_cut_inside_a_line() {
        let got = lines(b"ok\npartial", 64).await;
        assert_eq!(
            got[1].as_ref().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn the_report_messages_have_a_fixed_shape_and_unknown_kinds_still_parse() {
        let exit = AgentMessage::entrypoint_exited(127, "x: not found".into());
        assert_eq!(
            exit.to_line().unwrap(),
            b"{\"type\":\"entrypoint_exited\",\"status\":127,\"tail\":\"x: not found\"}\n"
        );
        let dns = AgentMessage::stub_dns(StubDnsTrouble::NoStandInAddress, 3);
        assert_eq!(
            dns.to_line().unwrap(),
            b"{\"type\":\"stub_dns\",\"kind\":\"no_stand_in_address\",\"count\":3}\n"
        );
        for kind in StubDnsTrouble::ALL {
            let msg = AgentMessage::stub_dns(kind, 1);
            assert_eq!(
                AgentMessage::from_line(&msg.to_line().unwrap()).unwrap(),
                msg
            );
        }
        assert_eq!(
            AgentMessage::from_line(
                b"{\"type\":\"stub_dns\",\"kind\":\"cosmic_rays\",\"count\":1}"
            )
            .unwrap(),
            AgentMessage::stub_dns(StubDnsTrouble::Unknown, 1)
        );
        assert_eq!(
            AgentMessage::from_line(b"{\"type\":\"entrypoint_exited\",\"status\":1}").unwrap(),
            AgentMessage::entrypoint_exited(1, String::new())
        );
        // A status no shell gives is not a message.
        assert!(
            AgentMessage::from_line(b"{\"type\":\"entrypoint_exited\",\"status\":256}").is_err()
        );
    }

    #[test]
    fn the_editor_status_round_trips_and_is_cut_to_its_limit() {
        let refused = AgentMessage::editor_status(Some("checksum mismatch"));
        assert_eq!(
            refused.to_line().unwrap(),
            b"{\"type\":\"editor_status\",\"problem\":\"checksum mismatch\"}\n"
        );
        assert_eq!(
            AgentMessage::from_line(&refused.to_line().unwrap()).unwrap(),
            refused
        );
        let fine = AgentMessage::editor_status(None);
        assert_eq!(fine.to_line().unwrap(), b"{\"type\":\"editor_status\"}\n");
        assert_eq!(
            AgentMessage::from_line(&fine.to_line().unwrap()).unwrap(),
            fine
        );
        let long = AgentMessage::editor_status(Some(&"\u{1b}".repeat(5000)));
        let AgentMessage::EditorStatus {
            problem: Some(text),
        } = &long
        else {
            panic!("a problem stays a problem");
        };
        assert_eq!(text.chars().count(), MAX_EDITOR_PROBLEM_CHARS);
        assert!(long.to_line().is_ok(), "the cut text fits a control line");
    }

    proptest! {
        #[test]
        fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = AgentMessage::from_line(&bytes);
        }

        #[test]
        fn oom_kill_lines_round_trip(pid in proptest::option::of(any::<u32>()), name in proptest::option::of(".{0,64}")) {
            let msg = AgentMessage::oom_kill(pid, name);
            let line = msg.to_line().unwrap();
            prop_assert_eq!(line.split(|&b| b == b'\n').count(), 2);
            prop_assert_eq!(AgentMessage::from_line(&line).unwrap(), msg);
        }
    }
}
