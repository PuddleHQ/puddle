// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent's side of the control stream: hello first, then every report from the OOM watch.
//! A failed write, or a stream the host or its session ended, reopens the stream (on a new session
//! if the old one died) and resends.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::AgentMessage;
use puddle_agent_proto::control::PREAMBLE;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_yamux::StreamHandle;

use crate::upstream::Upstream;

/// First wait before reopening a failed control stream; it doubles up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_millis(100);
/// Longest wait between attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// Keeps a control stream open and writes every message from `reports` to it. Returns when
/// `reports` closes.
pub async fn run(upstream: Arc<Upstream>, mut reports: mpsc::Receiver<AgentMessage>) {
    let mut pending: Option<AgentMessage> = None;
    let mut backoff = FIRST_BACKOFF;
    loop {
        let mut stream = match open(&upstream).await {
            Ok(stream) => {
                backoff = FIRST_BACKOFF;
                stream
            }
            Err(err) => {
                tracing::warn!(error = %err, retry_ms = backoff.as_millis(), "control stream to the host failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                continue;
            }
        };
        loop {
            let msg = if let Some(msg) = pending.take() {
                msg
            } else {
                tokio::select! {
                    next = reports.recv() => match next {
                        Some(msg) => msg,
                        None => return,
                    },
                    () = lost(&mut stream) => {
                        tracing::info!("control stream ended by the host or its session, reopening");
                        break;
                    }
                }
            };
            if let Err(err) = write(&mut stream, &msg).await {
                tracing::info!(error = %err, "control stream lost, reopening");
                pending = Some(msg);
                break;
            }
        }
    }
}

/// Resolves when the stream ends or fails. The host never writes on it, so a read that returns
/// (end of stream, an error, or a stray byte) means the stream or its session is gone, and the
/// host holds a dead link until a hello arrives again.
async fn lost(stream: &mut StreamHandle) {
    let _ = stream.read(&mut [0u8; 1]).await;
}

async fn open(upstream: &Upstream) -> io::Result<StreamHandle> {
    let mut stream = upstream.open().await?;
    stream.write_all(PREAMBLE).await?;
    write(&mut stream, &AgentMessage::hello()).await?;
    Ok(stream)
}

async fn write(stream: &mut StreamHandle, msg: &AgentMessage) -> io::Result<()> {
    let line = msg
        .to_line()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    stream.write_all(&line).await?;
    stream.flush().await
}
