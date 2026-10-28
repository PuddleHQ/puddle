// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent's end of the host's messages ([`puddle_agent_proto::host_control`]): reads one,
//! does what it asks, writes one reply.

use std::path::PathBuf;

use puddle_agent_proto::forward::{ANSWER_TIMEOUT, read_line_exact};
use puddle_agent_proto::host_control::{HostMessage, HostReply, MAX_LINE};
use tokio::io::AsyncWriteExt;
use tokio_yamux::StreamHandle;

use crate::editor::Control;
use crate::exthost;

/// Serves one `host-control` stream whose preamble [`crate::host_streams`] has read.
pub async fn serve(mut stream: StreamHandle, servers: &[PathBuf], control: Option<&Control>) {
    let line = tokio::time::timeout(ANSWER_TIMEOUT, read_line_exact(&mut stream, MAX_LINE)).await;
    let Ok(Ok(line)) = line else {
        tracing::debug!("a host-control stream sent no message in time and was dropped");
        return;
    };
    let reply = reply_to(
        HostMessage::from_line(&line).unwrap_or(HostMessage::Unknown),
        servers,
        control,
    )
    .await;
    // The reply is out; a close the host already made is not a problem.
    let _ = async {
        stream.write_all(&reply.to_line()).await?;
        stream.flush().await?;
        stream.shutdown().await
    }
    .await;
}

/// What the agent does for `message`, and the reply that says it is done.
pub async fn reply_to(
    message: HostMessage,
    servers: &[PathBuf],
    control: Option<&Control>,
) -> HostReply {
    match message {
        HostMessage::EndExtensionHosts => {
            let ended = exthost::end_extension_hosts(servers).await;
            tracing::info!(
                hosts = ended.hosts,
                processes = ended.processes,
                orphans = ended.orphans,
                "ended the editor's extension hosts"
            );
            HostReply::done(ended.hosts, ended.processes, ended.orphans)
        }
        HostMessage::SwitchEditorBuild => {
            let Some(control) = control else {
                return HostReply::Ignored;
            };
            match control.switch() {
                Ok(switched) => {
                    tracing::info!(
                        switched = switched.is_some(),
                        "asked to run the staged editor server build"
                    );
                    HostReply::EditorBuild {
                        switched: switched.is_some(),
                    }
                }
                Err(err) => {
                    tracing::warn!(%err, "could not switch the editor server build");
                    HostReply::EditorBuild { switched: false }
                }
            }
        }
        _ => HostReply::Ignored,
    }
}
