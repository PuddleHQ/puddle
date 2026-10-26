// SPDX-License-Identifier: GPL-3.0-or-later
//! The streams the host opens to the agent, told apart by their preamble
//! ([`puddle_agent_proto::kind`]): `forward` (a connection to a guest port, [`crate::forward`])
//! and `host-control` (a message to act on, [`crate::host_control`]). Anything else is dropped.

use std::path::PathBuf;
use std::sync::Arc;

use puddle_agent_proto::forward::{ANSWER_TIMEOUT, read_line_exact};
use puddle_agent_proto::kind::{MAX_PREAMBLE, StreamKind, parse_preamble};
use tokio_yamux::StreamHandle;

use crate::forward::Forwards;
use crate::host_control;

/// What serves the host's streams.
#[derive(Debug)]
pub struct HostStreams {
    forwards: Arc<Forwards>,
    /// The folders of the editor servers whose extension hosts a `host-control` message may end.
    servers: Vec<PathBuf>,
}

impl HostStreams {
    /// Serves forwards with `forwards` and ends the extension hosts of the servers in `servers`.
    #[must_use]
    pub fn new(forwards: Arc<Forwards>, servers: Vec<PathBuf>) -> Arc<Self> {
        Arc::new(Self { forwards, servers })
    }

    /// Serves one stream the host opened, until it ends. A stream with another or no valid
    /// preamble is dropped.
    pub async fn serve(&self, mut stream: StreamHandle) {
        let preamble =
            tokio::time::timeout(ANSWER_TIMEOUT, read_line_exact(&mut stream, MAX_PREAMBLE)).await;
        let Ok(Ok(preamble)) = preamble else {
            tracing::debug!("a stream from the host sent no preamble in time and was dropped");
            return;
        };
        match parse_preamble(&preamble) {
            Ok((StreamKind::Forward, 1)) => self.forwards.serve(stream).await,
            Ok((StreamKind::HostControl, 1)) => host_control::serve(stream, &self.servers).await,
            _ => tracing::debug!("a stream from the host with an unusable preamble was dropped"),
        }
    }
}
