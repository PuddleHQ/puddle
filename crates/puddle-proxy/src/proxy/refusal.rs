// SPDX-License-Identifier: AGPL-3.0-or-later
//! The error response a refused connection gets.

use puddle_types::ConnectionOutcome;
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// An error response: status, extra headers, a one-line explanation for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: std::borrow::Cow<'static, str>,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) message: String,
    /// The reason the connection record carries, for a refusal Puddle made on purpose (not an
    /// upstream failure): also sent as `x-puddle-blocked`.
    pub(crate) code: Option<&'static str>,
    /// How reaching the destination went, for a refusal that is a failure to get there (not a
    /// decision): the connection record's `outcome`.
    pub(crate) outcome: Option<ConnectionOutcome>,
}

impl Refusal {
    pub(crate) fn new(
        status: impl Into<std::borrow::Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: status.into(),
            headers: Vec::new(),
            message: message.into(),
            code: None,
            outcome: None,
        }
    }

    /// Marks this refusal as a failure to reach the destination, which the connection record
    /// carries as `outcome`.
    pub(crate) fn failed(mut self, outcome: ConnectionOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Marks this refusal as one Puddle made on purpose: the connection record gets `code` as its
    /// reason, and the guest reads it in `x-puddle-blocked`.
    pub(crate) fn coded(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self.header("x-puddle-blocked", code)
    }

    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        let body = format!("puddle: {}\n", self.message);
        let mut out = format!(
            "HTTP/1.1 {}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n",
            self.status,
            body.len()
        );
        for (name, value) in &self.headers {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("connection: close\r\n\r\n");
        out.push_str(&body);
        out.into_bytes()
    }
}

/// Sends `refusal` and closes the stream cleanly: a refusal is a normal end, and dropping the
/// stream without the shutdown would reset it, which can reach the client before the response.
pub(crate) async fn refuse<S: AsyncWrite + Unpin>(stream: &mut S, refusal: &Refusal) {
    let sent = async {
        stream.write_all(&refusal.bytes()).await?;
        stream.shutdown().await
    };
    if let Err(err) = sent.await {
        tracing::debug!(error = %err, "guest went away before the refusal was sent");
    }
}
