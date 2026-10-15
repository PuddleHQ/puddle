// SPDX-License-Identifier: GPL-3.0-or-later
//! The `forward` stream: the host reaches a port inside the guest.
//!
//! The one stream kind the **host** opens. It writes [`PREAMBLE`] and one line with the port
//! (`3000\n`); the agent connects to that port on the guest's loopback and answers with one line,
//! [`Answer::Ok`] (`ok\n`) or [`Answer::Refused`] (`refused <reason>\n`). After `ok` the stream
//! is the connection, byte for byte, until either side closes it.
//!
//! The host opens it on the session that carries the agent's control stream ([`GuestLink`]).
//! Nothing here is addressed by anything the guest controls: the host names a port, never an
//! address, so a forward can only ever reach the guest's own loopback.
//!
//! ```
//! use puddle_agent_proto::forward::{Answer, parse_request, request};
//! assert_eq!(request(3000), b"\0puddle-forward/1\n3000\n");
//! assert_eq!(parse_request(b"3000\n"), Ok(3000));
//! assert_eq!(Answer::Ok.to_line(), b"ok\n");
//! assert_eq!(Answer::from_line(b"refused nothing listens\n"), Ok(Answer::Refused("nothing listens".into())));
//! ```

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio_yamux::{Control, StreamHandle};

/// Bytes that open a forward stream ([`crate::kind::StreamKind::Forward`] v1).
pub const PREAMBLE: &[u8] = crate::kind::StreamKind::Forward.preamble();

/// Longest request line the agent reads, newline included (a port is at most five digits).
pub const MAX_REQUEST_LINE: usize = 16;

/// Longest answer line the host reads, newline included.
pub const MAX_ANSWER_LINE: usize = 256;

/// How long the host waits for the agent's answer, and the agent for the request line.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// A forward request or answer that can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LineError {
    /// Not a port number from 1 to 65535.
    #[error("not a port number")]
    BadPort,
    /// Not `ok` or `refused <reason>`.
    #[error("not an answer to a forward request")]
    BadAnswer,
}

/// The bytes the host writes to open a forward to `port`.
#[must_use]
pub fn request(port: u16) -> Vec<u8> {
    let mut bytes = PREAMBLE.to_vec();
    bytes.extend_from_slice(format!("{port}\n").as_bytes());
    bytes
}

/// The port in a request line (newline included or not).
///
/// # Errors
///
/// [`LineError::BadPort`] for anything but a port from 1 to 65535.
pub fn parse_request(line: &[u8]) -> Result<u16, LineError> {
    let text = std::str::from_utf8(line).map_err(|_| LineError::BadPort)?;
    let text = text.strip_suffix('\n').unwrap_or(text);
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(LineError::BadPort);
    }
    match text.parse::<u16>() {
        Ok(0) | Err(_) => Err(LineError::BadPort),
        Ok(port) => Ok(port),
    }
}

/// The agent's answer to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Connected: the stream is the connection from here on.
    Ok,
    /// Not connected, and why (the agent's own text; the host cleans it before use).
    Refused(String),
}

impl Answer {
    /// A refusal with `reason` cut to fit one line.
    #[must_use]
    pub fn refused(reason: &str) -> Self {
        Self::Refused(
            reason
                .chars()
                .filter(|c| !c.is_control())
                .take(MAX_ANSWER_LINE - 16)
                .collect(),
        )
    }

    /// The answer as one line, newline included.
    #[must_use]
    pub fn to_line(&self) -> Vec<u8> {
        match self {
            Self::Ok => b"ok\n".to_vec(),
            Self::Refused(reason) => format!("refused {reason}\n").into_bytes(),
        }
    }

    /// Parses an answer line (newline included or not).
    ///
    /// # Errors
    ///
    /// [`LineError::BadAnswer`] for anything else.
    pub fn from_line(line: &[u8]) -> Result<Self, LineError> {
        let text = std::str::from_utf8(line).map_err(|_| LineError::BadAnswer)?;
        let text = text.strip_suffix('\n').unwrap_or(text);
        if text == "ok" {
            return Ok(Self::Ok);
        }
        text.strip_prefix("refused ")
            .map(|reason| Self::refused(reason))
            .ok_or(LineError::BadAnswer)
    }
}

/// Reads one line a byte at a time, so nothing after it is consumed: the connection's own data
/// follows an answer at once.
///
/// # Errors
///
/// A read error, `UnexpectedEof` for a stream that ends inside the line, `InvalidData` for a line
/// past `max` bytes.
pub async fn read_line_exact<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> io::Result<Vec<u8>> {
    let mut line = Vec::with_capacity(32);
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await?;
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(line);
        }
        if line.len() >= max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
        }
    }
}

/// Why the host could not open a forward.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum OpenError {
    /// The agent's session is gone.
    #[error("the workspace's agent is not connected")]
    Disconnected,
    /// The agent did not answer in time.
    #[error("the workspace's agent did not answer in {0:?}")]
    Timeout(Duration),
    /// The agent could not connect to the port.
    #[error("{0}")]
    Refused(String),
    /// The stream failed or the answer was not one.
    #[error("the agent's answer could not be read: {0}")]
    Io(String),
}

/// The host's handle on one agent session: opens streams to the agent. Cheap to clone.
#[derive(Clone)]
pub struct GuestLink {
    control: Control,
}

impl std::fmt::Debug for GuestLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestLink").finish_non_exhaustive()
    }
}

impl GuestLink {
    pub(crate) fn new(control: Control) -> Self {
        Self { control }
    }

    /// Opens a connection to `port` on the guest's loopback. The returned stream carries the
    /// connection's bytes.
    ///
    /// # Errors
    ///
    /// [`OpenError`] says whether the agent is gone, silent, or could not connect.
    pub async fn open_forward(&self, port: u16) -> Result<StreamHandle, OpenError> {
        let mut control = self.control.clone();
        let mut stream = control
            .open_stream()
            .await
            .map_err(|_| OpenError::Disconnected)?;
        let exchange = async {
            stream.write_all(&request(port)).await?;
            stream.flush().await?;
            read_line_exact(&mut stream, MAX_ANSWER_LINE).await
        };
        let line = tokio::time::timeout(ANSWER_TIMEOUT, exchange)
            .await
            .map_err(|_| OpenError::Timeout(ANSWER_TIMEOUT))?
            .map_err(|e| OpenError::Io(e.to_string()))?;
        match Answer::from_line(&line) {
            Ok(Answer::Ok) => Ok(stream),
            Ok(Answer::Refused(reason)) => Err(OpenError::Refused(reason)),
            Err(e) => Err(OpenError::Io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_the_preamble_and_the_port() {
        assert_eq!(request(65_535), b"\0puddle-forward/1\n65535\n");
        assert!(PREAMBLE.len() <= crate::kind::MAX_PREAMBLE);
    }

    #[test]
    fn only_a_plain_port_number_parses() {
        assert_eq!(parse_request(b"8080\n"), Ok(8080));
        assert_eq!(parse_request(b"1"), Ok(1));
        for bad in [
            &b""[..],
            b"\n",
            b"0\n",
            b"65536\n",
            b"-1\n",
            b"+80\n",
            b" 80\n",
            b"80 \n",
            b"0x50\n",
            b"\xff\n",
            b"1e3\n",
        ] {
            assert_eq!(parse_request(bad), Err(LineError::BadPort), "{bad:?}");
        }
    }

    #[test]
    fn answers_round_trip_and_a_reason_is_one_clean_line() {
        assert_eq!(Answer::from_line(&Answer::Ok.to_line()), Ok(Answer::Ok));
        let refused = Answer::refused("nothing\r\nlistens\x07 here");
        assert_eq!(refused, Answer::Refused("nothinglistens here".into()));
        assert_eq!(Answer::from_line(&refused.to_line()), Ok(refused));
        for bad in [&b"yes\n"[..], b"", b"refused\n", b"\xff", b"OK\n"] {
            assert_eq!(Answer::from_line(bad), Err(LineError::BadAnswer), "{bad:?}");
        }
        assert!(Answer::refused(&"x".repeat(10_000)).to_line().len() <= MAX_ANSWER_LINE);
    }

    #[tokio::test]
    async fn a_line_is_read_without_taking_what_follows() {
        let mut input: &[u8] = b"ok\nDATA";
        assert_eq!(read_line_exact(&mut input, 16).await.unwrap(), b"ok\n");
        assert_eq!(input, b"DATA");
        let mut long: &[u8] = b"aaaaaaaaaaaaaaaaaaaa\n";
        assert_eq!(
            read_line_exact(&mut long, 8).await.unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut cut: &[u8] = b"ok";
        assert_eq!(
            read_line_exact(&mut cut, 8).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
