// SPDX-License-Identifier: GPL-3.0-or-later
//! The `editor` stream: the host reaches the guest's editor server.
//!
//! Opened by the **host** on the session that carries the agent's control stream
//! ([`crate::forward::GuestLink`]). It writes [`PREAMBLE`] and one request line:
//!
//! | Request | Answer |
//! |---|---|
//! | `connect\n` | `ok\n`, then the stream is a connection to the editor server, byte for byte; or `refused <reason>\n` |
//! | `password\n` | `password <hex>\n` and the stream ends; or `refused <reason>\n` |
//!
//! The host names no port and no address: the agent connects to the one port it runs the editor
//! server on, so this stream cannot reach anything else in the guest, and a `forward` stream can
//! still refuse that port.
//!
//! ```
//! use puddle_agent_proto::editor_stream::{Request, request};
//! assert_eq!(request(Request::Connect), b"\0puddle-editor/1\nconnect\n");
//! assert_eq!(Request::parse(b"password\n"), Ok(Request::Password));
//! ```

use tokio::io::AsyncWriteExt;
use tokio_yamux::StreamHandle;

use crate::forward::{
    ANSWER_TIMEOUT, Answer, GuestLink, MAX_ANSWER_LINE, OpenError, read_line_exact,
};

/// Bytes that open an editor stream ([`crate::kind::StreamKind::Editor`] v1).
pub const PREAMBLE: &[u8] = crate::kind::StreamKind::Editor.preamble();

/// Longest request line the agent reads, newline included.
pub const MAX_REQUEST_LINE: usize = 16;

/// What the host asks of the editor stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// A connection to the editor server.
    Connect,
    /// The editor server's password.
    Password,
}

impl Request {
    /// The request in a line (newline included or not).
    ///
    /// # Errors
    ///
    /// [`BadRequest`] for anything but `connect` or `password`.
    pub fn parse(line: &[u8]) -> Result<Self, BadRequest> {
        match line.strip_suffix(b"\n").unwrap_or(line) {
            b"connect" => Ok(Self::Connect),
            b"password" => Ok(Self::Password),
            _ => Err(BadRequest),
        }
    }

    const fn word(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Password => "password",
        }
    }
}

/// A request line that is neither `connect` nor `password`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not an editor request")]
pub struct BadRequest;

/// The bytes the host writes to open an editor stream with `request`.
#[must_use]
pub fn request(request: Request) -> Vec<u8> {
    let mut bytes = PREAMBLE.to_vec();
    bytes.extend_from_slice(request.word().as_bytes());
    bytes.push(b'\n');
    bytes
}

/// The answer line to a [`Request::Password`] request.
#[must_use]
pub fn password_line(password: &str) -> Vec<u8> {
    format!("password {password}\n").into_bytes()
}

/// Whether `password` is what the agent makes: lowercase hex, never empty, short enough for a line.
fn is_password(password: &str) -> bool {
    !password.is_empty()
        && password.len() <= MAX_ANSWER_LINE - 16
        && password.bytes().all(|b| b.is_ascii_hexdigit())
}

impl GuestLink {
    async fn open_editor_stream(
        &self,
        what: Request,
    ) -> Result<(StreamHandle, Vec<u8>), OpenError> {
        let mut control = self.control.clone();
        let mut stream = control
            .open_stream()
            .await
            .map_err(|_| OpenError::Disconnected)?;
        let exchange = async {
            stream.write_all(&request(what)).await?;
            stream.flush().await?;
            read_line_exact(&mut stream, MAX_ANSWER_LINE).await
        };
        let line = tokio::time::timeout(ANSWER_TIMEOUT, exchange)
            .await
            .map_err(|_| OpenError::Timeout(ANSWER_TIMEOUT))?
            .map_err(|e| OpenError::Io(e.to_string()))?;
        Ok((stream, line))
    }

    /// Opens a connection to the guest's editor server. The returned stream carries the
    /// connection's bytes.
    ///
    /// # Errors
    ///
    /// [`OpenError`] says whether the agent is gone, silent, or has no server to connect to.
    pub async fn open_editor(&self) -> Result<StreamHandle, OpenError> {
        let (stream, line) = self.open_editor_stream(Request::Connect).await?;
        match Answer::from_line(&line) {
            Ok(Answer::Ok) => Ok(stream),
            Ok(Answer::Refused(reason)) => Err(OpenError::Refused(reason)),
            Err(e) => Err(OpenError::Io(e.to_string())),
        }
    }

    /// The editor server's password for this boot of the sandbox.
    ///
    /// # Errors
    ///
    /// [`OpenError`] says whether the agent is gone, silent, has no server, or answered with
    /// something that is not a password.
    pub async fn editor_password(&self) -> Result<String, OpenError> {
        let (_stream, line) = self.open_editor_stream(Request::Password).await?;
        let text = std::str::from_utf8(&line)
            .map_err(|_| OpenError::Io("the answer is not text".into()))?
            .trim_end_matches('\n');
        if let Some(password) = text.strip_prefix("password ") {
            return if is_password(password) {
                Ok(password.to_owned())
            } else {
                Err(OpenError::Io(
                    "the password is not one the agent makes".into(),
                ))
            };
        }
        match Answer::from_line(&line) {
            Ok(Answer::Refused(reason)) => Err(OpenError::Refused(reason)),
            _ => Err(OpenError::Io("not an answer to a password request".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_the_preamble_and_one_word() {
        assert_eq!(request(Request::Connect), b"\0puddle-editor/1\nconnect\n");
        assert_eq!(request(Request::Password), b"\0puddle-editor/1\npassword\n");
        assert!(PREAMBLE.len() <= crate::kind::MAX_PREAMBLE);
    }

    #[test]
    fn only_the_two_words_parse() {
        assert_eq!(Request::parse(b"connect"), Ok(Request::Connect));
        for bad in [
            &b""[..],
            b"\n",
            b"Connect\n",
            b"connect 41337\n",
            b"41337\n",
            b"password\r\n",
        ] {
            assert_eq!(Request::parse(bad), Err(BadRequest), "{bad:?}");
        }
    }

    #[test]
    fn a_password_is_hex_and_fits_a_line() {
        assert!(is_password(&"ab".repeat(32)));
        for bad in ["", "xyz", "ab cd", &"a".repeat(MAX_ANSWER_LINE)] {
            assert!(!is_password(bad), "{bad:?}");
        }
        assert_eq!(password_line("ab"), b"password ab\n");
    }
}
