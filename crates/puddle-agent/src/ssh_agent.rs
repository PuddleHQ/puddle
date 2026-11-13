// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest's SSH agent socket: each client becomes one `ssh-agent` stream to the host, whose
//! filter answers it. Nothing here reads the protocol, so a client that talks to the stream
//! another way gets the same answers.
//!
//! The socket belongs to the session user, mode 0600. The user may not exist yet when the agent
//! starts (the boot hook makes it), so the owner is set as soon as the account can be read.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::kind::StreamKind;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{UnixListener, UnixStream};

use crate::account::Account;
use crate::config::SshAgentConfig;
use crate::upstream::Upstream;

/// How often the session user is looked for.
const OWNER_POLL: Duration = Duration::from_millis(250);

/// Binds the socket: an old one (from an earlier agent) is replaced, its folder made, its mode
/// 0600.
///
/// # Errors
///
/// When the folder can't be made or the socket can't be bound.
pub fn bind(socket: &Path) -> io::Result<UnixListener> {
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(socket) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(listener)
}

/// Gives the socket to the session user once its account exists; `false` when it never did
/// within the wait or the owner could not be set (the socket then stays root's).
pub async fn own(config: &SshAgentConfig) -> bool {
    let deadline = tokio::time::Instant::now() + config.owner_wait;
    loop {
        match Account::read(&config.user) {
            Ok(account) => {
                return match account.own(&config.socket) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(socket = %config.socket.display(), user = %config.user, error = %e, "the ssh agent socket stays root's");
                        false
                    }
                };
            }
            Err(e) if tokio::time::Instant::now() >= deadline => {
                tracing::warn!(user = %config.user, error = %e, "no session user for the ssh agent socket; it stays root's");
                return false;
            }
            Err(_) => tokio::time::sleep(OWNER_POLL).await,
        }
    }
}

/// Serves the socket: binds it, sets its owner, then carries each client to the host.
pub async fn serve(config: SshAgentConfig, upstream: Arc<Upstream>) {
    let listener = match bind(&config.socket) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::warn!(socket = %config.socket.display(), error = %e, "the ssh agent socket cannot be made; ssh in the workspace has no agent");
            return;
        }
    };
    tracing::info!(socket = %config.socket.display(), "ssh agent socket listening");
    tokio::spawn({
        let config = config.clone();
        async move { own(&config).await }
    });
    accept(listener, upstream).await;
}

/// Carries each client of `listener` to the host until the listener fails.
pub async fn accept(listener: UnixListener, upstream: Arc<Upstream>) {
    loop {
        match listener.accept().await {
            Ok((client, _)) => {
                tokio::spawn(relay(client, Arc::clone(&upstream)));
            }
            Err(e) => {
                tracing::warn!(error = %e, "the ssh agent socket stopped accepting");
                return;
            }
        }
    }
}

/// One client: an `ssh-agent` stream to the host, its bytes both ways until either side ends.
async fn relay(mut client: UnixStream, upstream: Arc<Upstream>) {
    let mut stream = match upstream.open().await {
        Ok(stream) => stream,
        Err(e) => {
            tracing::warn!(error = %e, "no stream to the host for an ssh agent client; closed");
            return;
        }
    };
    if stream
        .write_all(StreamKind::SshAgent.preamble())
        .await
        .is_err()
    {
        return;
    }
    if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut stream).await {
        tracing::debug!(error = %e, "an ssh agent client ended with an error");
    }
}

#[cfg(test)]
mod tests;
