// SPDX-License-Identifier: AGPL-3.0-or-later
//! A terminal's session: the host's own SSH connection to the runtime's SSH server over an
//! in-memory stream, signed in with the login key, with a pseudo-terminal and a login shell.
//!
//! Nothing listens on the user's computer for it. The runtime's server gives the session user a
//! plain shell in `/` for any SSH client, so the session asks for a command that goes home and
//! becomes the account's login shell (`LOGIN_SHELL`): that is what brings the workspace's
//! environment and shell init, and the start folder.
//! The session is a [`TerminalLink`]: bytes and control in, bytes and the end out. No terminal
//! byte is logged.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_api::{
    EndReason, MAX_MESSAGE_BYTES, TerminalEnd, TerminalError, TerminalInput, TerminalLink,
    TerminalOutput, TerminalSize, WINDOW_BYTES,
};
use puddle_types::WorkspaceId;
use russh::client::{self, Handler};
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};
use russh::{ChannelMsg, Disconnect};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// How long a failed sign-in waits for the server to say why it refused.
/// What the runtime's SSH server is asked to run: it starts a plain `/bin/sh` in `/` for a shell
/// request, and only an interactive login shell that starts in the home directory is moved to the
/// start folder (and reads the workspace's environment and shell init). So go home, then become
/// the account's own login shell.
const LOGIN_SHELL: &str =
    "cd 2>/dev/null; s=$(getent passwd \"$(id -un)\" | cut -d: -f7); exec \"${s:-/bin/sh}\" -l";

const REFUSAL_WAIT: Duration = Duration::from_millis(500);

/// What the host signs in with: the login key of direct SSH, read into memory.
#[derive(Clone)]
pub(crate) struct Identity(Arc<PrivateKey>);

impl Identity {
    /// Reads the private key file.
    pub(crate) fn load(path: &Path) -> Result<Self, String> {
        russh::keys::load_secret_key(path, None)
            .map(|key| Self(Arc::new(key)))
            .map_err(|e| format!("the login key cannot be read: {e}"))
    }
}

/// The sessions open now, so a stop can end them with its reason before the VM goes.
#[derive(Default)]
pub(crate) struct Hub {
    open: Mutex<Vec<(WorkspaceId, watch::Sender<Option<EndReason>>)>>,
}

impl Hub {
    fn register(&self, workspace: &WorkspaceId) -> watch::Receiver<Option<EndReason>> {
        let (tx, rx) = watch::channel(None);
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.retain(|(_, sender)| !sender.is_closed());
        open.push((workspace.clone(), tx));
        rx
    }

    /// How many sessions are open now, in every workspace.
    pub(crate) fn count(&self) -> usize {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.iter()
            .filter(|(_, sender)| !sender.is_closed())
            .count()
    }

    /// Ends every session of `workspace` with `reason`.
    pub(crate) fn end(&self, workspace: &WorkspaceId, reason: EndReason) {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        for (name, sender) in open.iter() {
            if name == workspace {
                sender.send_replace(Some(reason));
            }
        }
    }
}

/// How much output is out with the page and not yet acknowledged.
#[derive(Debug, Default)]
struct Window {
    sent: u64,
    acked: u64,
}

impl Window {
    /// How many more bytes may go out now.
    fn room(&self) -> u64 {
        WINDOW_BYTES.saturating_sub(self.sent - self.acked)
    }

    fn sent(&mut self, bytes: u64) {
        self.sent += bytes;
    }

    /// The page drew `bytes` more; it cannot have drawn more than was sent.
    fn ack(&mut self, bytes: u64) {
        self.acked = self.acked.saturating_add(bytes).min(self.sent);
    }
}

/// Trusts the server: its other end is this process.
struct InMemory;

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's method is async; the answer is known"
)]
impl Handler for InMemory {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// What opens a terminal's session.
pub(crate) struct Open<'a> {
    /// Whose terminal it is, so a stop can end it.
    pub(crate) workspace: &'a WorkspaceId,
    /// The login key.
    pub(crate) identity: &'a Identity,
    /// The session user.
    pub(crate) user: &'a str,
    /// The first size.
    pub(crate) size: TerminalSize,
}

/// Opens a session: `serve` runs the runtime's SSH server on the stream it is given (it returns
/// when the connection ends), and `gone` says why a connection that ended without an exit status
/// did (the workspace stopped or crashed). The link is returned once the shell has started.
pub(crate) async fn open<F, Fut, G>(
    hub: &Hub,
    request: Open<'_>,
    serve: F,
    gone: G,
) -> Result<TerminalLink, TerminalError>
where
    F: FnOnce(tokio::io::DuplexStream) -> Fut,
    Fut: Future<Output = Result<(), String>> + Send + 'static,
    G: Fn() -> EndReason + Send + Sync + 'static,
{
    let (near, far) = tokio::io::duplex(65_536);
    let mut server = tokio::spawn(serve(far));
    let started = tokio::select! {
        ended = &mut server => {
            // The server ended before a shell started: its refusal is the reason.
            let message = match ended {
                Ok(Err(reason)) => reason,
                Ok(Ok(())) => "the workspace's SSH server closed the connection".to_owned(),
                Err(e) => e.to_string(),
            };
            return Err(TerminalError::Ended(TerminalEnd {
                reason: EndReason::BootFailed,
                status: None,
                message: Some(puddle_ssh::sanitize_reason(&message)),
            }));
        }
        started = start(near, request.identity, request.user, request.size) => started,
    };
    let (handle, channel) = match started {
        Ok(started) => started,
        Err(failure) => {
            // A server that ended the connection says why itself; one that answered with a
            // refusal is quoted as the client heard it (the client leaving ends the server with
            // an error of its own).
            let refusal = match failure {
                Failure::Broke(text) => {
                    match tokio::time::timeout(REFUSAL_WAIT, &mut server).await {
                        Ok(Ok(Err(reason))) => reason,
                        _ => text,
                    }
                }
                Failure::Refused(text) => text,
            };
            server.abort();
            return Err(TerminalError::Ended(TerminalEnd {
                reason: EndReason::BootFailed,
                status: None,
                message: Some(puddle_ssh::sanitize_reason(&refusal)),
            }));
        }
    };
    let (input, from_page) = mpsc::channel(64);
    let (to_page, output) = mpsc::channel(64);
    let ending = hub.register(request.workspace);
    tokio::spawn(async move {
        let end = pump(channel, from_page, &to_page, ending, &mut server, &gone).await;
        let _ = handle
            .disconnect(Disconnect::ByApplication, "terminal closed", "en")
            .await;
        server.abort();
        let _ = to_page.send(TerminalOutput::Ended(end)).await;
    });
    Ok(TerminalLink { input, output })
}

type Shell = russh::Channel<client::Msg>;

/// Why the client could not get a shell.
enum Failure {
    /// The connection broke: the server may say why.
    Broke(String),
    /// The server answered with a refusal.
    Refused(String),
}

async fn start<S>(
    stream: S,
    identity: &Identity,
    user: &str,
    size: TerminalSize,
) -> Result<(client::Handle<InMemory>, Shell), Failure>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // An idle shell is normal: russh's default has no inactivity timeout, and this keeps it so.
    let config = Arc::new(client::Config::default());
    let mut handle = client::connect_stream(config, stream, InMemory)
        .await
        .map_err(|e| Failure::Broke(format!("the SSH connection failed: {e}")))?;
    let signed_in = handle
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(identity.0.clone(), None))
        .await
        .map_err(|e| Failure::Broke(format!("signing in failed: {e}")))?;
    if !signed_in.success() {
        return Err(Failure::Refused(format!(
            "the workspace's SSH server refused the login as {user}"
        )));
    }
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| Failure::Broke(format!("a session could not be opened: {e}")))?;
    channel
        .request_pty(
            true,
            "xterm-256color",
            u32::from(size.cols()),
            u32::from(size.rows()),
            0,
            0,
            &[],
        )
        .await
        .map_err(|e| Failure::Broke(e.to_string()))?;
    expect_success(&mut channel, "a pseudo-terminal").await?;
    // Not every server takes variables; the shell works without.
    let _ = channel.set_env(false, "COLORTERM", "truecolor").await;
    channel
        .exec(true, LOGIN_SHELL)
        .await
        .map_err(|e| Failure::Broke(e.to_string()))?;
    expect_success(&mut channel, "a shell").await?;
    Ok((handle, channel))
}

/// The reason a connection that ended without an exit status gives, from the workspace's status.
pub(crate) fn reason_for(status: Option<puddle_types::WorkspaceStatus>) -> EndReason {
    match status {
        Some(puddle_types::WorkspaceStatus::Crashed) => EndReason::Crashed,
        _ => EndReason::Stopped,
    }
}

/// Looks with `probe` every `every` until it finds something, an error ends the wait, and
/// `limit` without a find ends it as a boot failure naming the seconds.
pub(crate) async fn wait_until_ready<T, F, Fut>(
    limit: Duration,
    every: Duration,
    mut probe: F,
) -> Result<T, TerminalError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Option<T>, TerminalError>>,
{
    let waiting = async {
        loop {
            if let Some(found) = probe().await? {
                return Ok(found);
            }
            tokio::time::sleep(every).await;
        }
    };
    tokio::time::timeout(limit, waiting)
        .await
        .unwrap_or_else(|_| {
            Err(TerminalError::Ended(TerminalEnd {
                reason: EndReason::BootFailed,
                status: None,
                message: Some(format!(
                    "the workspace was not ready in {} seconds",
                    limit.as_secs()
                )),
            }))
        })
}

async fn expect_success(channel: &mut Shell, what: &str) -> Result<(), Failure> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => {
                return Err(Failure::Refused(format!(
                    "the workspace's SSH server refused to give {what}"
                )));
            }
            Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(Failure::Refused(format!(
                    "the connection ended before {what} was given"
                )));
            }
            Some(_) => {}
        }
    }
}

/// Moves bytes between the page and the shell until the session ends; returns how it ended.
async fn pump(
    mut channel: Shell,
    mut from_page: mpsc::Receiver<TerminalInput>,
    to_page: &mpsc::Sender<TerminalOutput>,
    mut ending: watch::Receiver<Option<EndReason>>,
    server: &mut JoinHandle<Result<(), String>>,
    gone: &(impl Fn() -> EndReason + Send + Sync),
) -> TerminalEnd {
    let mut window = Window::default();
    let mut backlog: Vec<u8> = Vec::new();
    let mut status: Option<u32> = None;
    // Set when the shell's side closed; the output it left is still delivered first.
    let mut finished: Option<TerminalEnd> = None;
    let mut server_ended = false;
    loop {
        if backlog.is_empty()
            && let Some(end) = finished.take()
        {
            return end;
        }
        let release = window.room().min(backlog.len() as u64);
        tokio::select! {
            () = async { let _ = ending.wait_for(Option::is_some).await; } => {
                let reason = (*ending.borrow()).unwrap_or(EndReason::Stopped);
                return TerminalEnd::of(reason);
            }
            input = from_page.recv() => match input {
                // The page's socket went away.
                None => return TerminalEnd::of(EndReason::Exited),
                Some(TerminalInput::Bytes(bytes)) => {
                    // A closed channel is noticed on the read side, which ends the session.
                    let _ = channel.data_bytes(bytes).await;
                }
                Some(TerminalInput::Resize(size)) => {
                    let _ = channel
                        .window_change(u32::from(size.cols()), u32::from(size.rows()), 0, 0)
                        .await;
                }
                Some(TerminalInput::Ack(bytes)) => window.ack(bytes),
            },
            _ = &mut *server, if !server_ended => {
                // The runtime's SSH server is gone: the workspace stopped or crashed.
                server_ended = true;
                finished.get_or_insert_with(|| end_of(status, gone));
            }
            permit = to_page.reserve(), if release > 0 => {
                let Ok(permit) = permit else {
                    return TerminalEnd::of(EndReason::Exited);
                };
                let take = usize::try_from(release).unwrap_or(usize::MAX).min(MAX_MESSAGE_BYTES);
                let chunk: Vec<u8> = backlog.drain(..take).collect();
                window.sent(chunk.len() as u64);
                permit.send(TerminalOutput::Bytes(chunk));
            }
            message = channel.wait(), if backlog.is_empty() && finished.is_none() => match message {
                Some(ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. }) => {
                    backlog.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => status = Some(exit_status),
                Some(ChannelMsg::ExitSignal { .. }) => status = status.or(Some(255)),
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => finished = Some(end_of(status, gone)),
                Some(_) => {}
            },
        }
    }
}

fn end_of(status: Option<u32>, gone: &impl Fn() -> EndReason) -> TerminalEnd {
    match status {
        Some(status) => TerminalEnd {
            reason: EndReason::Exited,
            status: Some(status),
            message: None,
        },
        None => TerminalEnd::of(gone()),
    }
}

#[cfg(test)]
mod tests;
