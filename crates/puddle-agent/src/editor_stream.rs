// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest end of the host's `editor` streams ([`puddle_agent_proto::editor_stream`]): the host
//! asks for a connection to the editor server, or for its password, and nothing else.
//!
//! The agent connects to the one port it runs the editor server on, so the host never names a port
//! here. A sandbox that runs no editor server answers every request with a reason.

use std::path::PathBuf;

use puddle_agent_proto::editor_stream::{MAX_REQUEST_LINE, Request, password_line};
use puddle_agent_proto::forward::{ANSWER_TIMEOUT, Answer, read_line_exact};
use puddle_agent_proto::relay::splice;
use tokio::io::AsyncWriteExt;
use tokio_yamux::StreamHandle;

use crate::editor::EditorConfig;
use crate::forward::{answer, connect};

/// What the host may reach of the editor server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorAccess {
    /// The server's port on the guest's loopback.
    pub port: u16,
    /// The file the server's password is in.
    pub password_file: PathBuf,
}

impl EditorAccess {
    /// The access for the server `config` runs.
    #[must_use]
    pub fn of(config: &EditorConfig) -> Self {
        Self {
            port: config.server.port,
            password_file: config.password_file.clone(),
        }
    }
}

const NO_SERVER: &str = "the workspace runs no editor server";

/// Serves one `editor` stream whose preamble [`crate::host_streams`] has read.
pub async fn serve(mut stream: StreamHandle, access: Option<&EditorAccess>) {
    let line = tokio::time::timeout(
        ANSWER_TIMEOUT,
        read_line_exact(&mut stream, MAX_REQUEST_LINE),
    )
    .await;
    let Ok(Ok(line)) = line else {
        tracing::debug!("an editor stream sent no request in time and was dropped");
        return;
    };
    let Ok(request) = Request::parse(&line) else {
        tracing::debug!("an editor stream with an unusable request was dropped");
        return;
    };
    let Some(access) = access else {
        answer(&mut stream, &Answer::refused(NO_SERVER)).await;
        return;
    };
    match request {
        Request::Connect => connect_through(stream, access.port).await,
        Request::Password => send_password(stream, access).await,
    }
}

async fn connect_through(mut stream: StreamHandle, port: u16) {
    let mut tcp = match connect(port).await {
        Ok(tcp) => tcp,
        Err(reason) => {
            answer(&mut stream, &Answer::refused(&reason)).await;
            return;
        }
    };
    if !answer(&mut stream, &Answer::Ok).await {
        return;
    }
    if let Err(err) = splice(&mut tcp, &mut stream).await {
        tracing::debug!(error = %err, "an editor connection ended with an error");
    }
}

async fn send_password(mut stream: StreamHandle, access: &EditorAccess) {
    let Ok(password) = tokio::fs::read_to_string(&access.password_file).await else {
        answer(
            &mut stream,
            &Answer::refused("the editor server has not started yet"),
        )
        .await;
        return;
    };
    // The file holds the password and nothing else; a stray newline must not reach the line.
    let password = password.trim();
    let sent = async {
        stream.write_all(&password_line(password)).await?;
        stream.flush().await?;
        stream.shutdown().await
    }
    .await;
    if let Err(err) = sent {
        tracing::debug!(error = %err, "the editor password could not be sent");
    }
}
