// SPDX-License-Identifier: GPL-3.0-or-later
//! The `host-control` stream: the host tells the agent to do something in the guest.
//!
//! The host opens one stream per message ([`crate::kind::StreamKind::HostControl`]), writes
//! [`PREAMBLE`] and one JSON [`HostMessage`] line, and reads one JSON [`HostReply`] line. The
//! agent does the work before it replies, so a reply means the work is done. No state outlives a
//! stream, which is why a session that dies and is redialled costs nothing to resync.
//!
//! Like the control stream, the format ignores what it doesn't know: an agent answers a message
//! type it doesn't know with [`HostReply::Ignored`], and a host reads a reply type it doesn't know
//! as [`HostReply::Unknown`].
//!
//! ```
//! use puddle_agent_proto::host_control::{HostMessage, HostReply};
//! assert_eq!(HostMessage::EndExtensionHosts.to_line(), b"{\"type\":\"end_extension_hosts\"}\n");
//! let reply = HostReply::done(2, 7, 1);
//! assert_eq!(HostReply::from_line(&reply.to_line()), Ok(reply));
//! ```

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::control::LineError;
use crate::forward::{GuestLink, OpenError, read_line_exact};

/// Bytes that open a host-control stream ([`crate::kind::StreamKind::HostControl`] v1).
pub const PREAMBLE: &[u8] = crate::kind::StreamKind::HostControl.preamble();

/// Longest message or reply line, newline included.
pub const MAX_LINE: usize = 1024;

/// How long the host waits for the reply: the agent ends a process tree before it answers (a
/// `TERM`, three seconds of grace, a `KILL`), so this is longer than a forward's answer.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// One message from the host to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    /// The sandbox's editor window closed: end the extension hosts of the editor servers in the
    /// guest and everything they started (language servers, debuggees), and leave the terminals
    /// (the servers' pty hosts) and everything else alone.
    EndExtensionHosts,
    /// No window is connected to the sandbox's editor server: if a newer build of Microsoft's
    /// server is downloaded and waiting, restart the server on it. Does nothing (and says so)
    /// when none is waiting, and for a sandbox that runs another server.
    SwitchEditorBuild,
    /// A message type this side doesn't know. Answered with [`HostReply::Ignored`].
    #[serde(other)]
    Unknown,
}

/// The agent's answer to one [`HostMessage`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostReply {
    /// [`HostMessage::EndExtensionHosts`] is done: no extension host of the servers is left.
    Done {
        /// Extension hosts found.
        hosts: u32,
        /// Processes ended in their trees, the hosts included.
        processes: u32,
        /// Debuggees found without a parent that could still use them, and ended.
        #[serde(default)]
        orphans: u32,
    },
    /// [`HostMessage::SwitchEditorBuild`] is done.
    EditorBuild {
        /// Whether the server was restarted on a newer build.
        switched: bool,
    },
    /// The agent doesn't know the message and did nothing.
    Ignored,
    /// A reply type this side doesn't know (from a newer agent).
    #[serde(other)]
    Unknown,
}

fn line<T: Serialize>(value: &T) -> Vec<u8> {
    // These types hold numbers and a tag: serialising cannot fail, and the line is far below the cap.
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

impl HostMessage {
    /// The message as one line, newline included.
    #[must_use]
    pub fn to_line(&self) -> Vec<u8> {
        line(self)
    }

    /// Parses one line (with or without its newline).
    ///
    /// # Errors
    ///
    /// [`LineError::Json`] if the line isn't a JSON object with a string `type`.
    pub fn from_line(bytes: &[u8]) -> Result<Self, LineError> {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        serde_json::from_slice(bytes).map_err(|e| LineError::Json(e.to_string()))
    }
}

impl HostReply {
    /// A [`HostReply::Done`].
    #[must_use]
    pub fn done(hosts: u32, processes: u32, orphans: u32) -> Self {
        Self::Done {
            hosts,
            processes,
            orphans,
        }
    }

    /// The reply as one line, newline included.
    #[must_use]
    pub fn to_line(&self) -> Vec<u8> {
        line(self)
    }

    /// Parses one line (with or without its newline).
    ///
    /// # Errors
    ///
    /// [`LineError::Json`] if the line isn't a JSON object with a string `type`.
    pub fn from_line(bytes: &[u8]) -> Result<Self, LineError> {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        serde_json::from_slice(bytes).map_err(|e| LineError::Json(e.to_string()))
    }
}

impl GuestLink {
    /// Sends `message` to the agent on a new `host-control` stream and returns its reply, which
    /// comes once the agent has done what the message asks.
    ///
    /// # Errors
    ///
    /// [`OpenError`] says whether the agent is gone, did not answer in [`REPLY_TIMEOUT`], or
    /// answered with something that is not a reply.
    pub async fn send(&self, message: &HostMessage) -> Result<HostReply, OpenError> {
        let mut control = self.control.clone();
        let mut stream = control
            .open_stream()
            .await
            .map_err(|_| OpenError::Disconnected)?;
        let exchange = async {
            stream.write_all(PREAMBLE).await?;
            stream.write_all(&message.to_line()).await?;
            stream.flush().await?;
            read_line_exact(&mut stream, MAX_LINE).await
        };
        let bytes = tokio::time::timeout(REPLY_TIMEOUT, exchange)
            .await
            .map_err(|_| OpenError::Timeout(REPLY_TIMEOUT))?
            .map_err(|e| OpenError::Io(e.to_string()))?;
        HostReply::from_line(&bytes).map_err(|e| OpenError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests;
