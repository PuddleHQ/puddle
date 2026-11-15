// SPDX-License-Identifier: AGPL-3.0-or-later
//! Terminals: the [`TerminalService`] trait the routes call (a shell session in a running
//! workspace), the registry of tickets and limits, [`NoTerminals`] for a build without it and
//! [`FakeTerminals`] for tests and the UI fixture.
//!
//! A session is a pair of channels ([`TerminalLink`]): bytes and control in, bytes and the end
//! out. The socket route moves messages between the page and the link; the service behind it
//! decides how a shell is reached and why a session ended.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use tokio::sync::mpsc;
use tokio::time::Instant;

/// How long a ticket works.
pub const TICKET_LIFETIME: Duration = Duration::from_secs(30);
/// Terminals one workspace can have open.
pub const MAX_PER_WORKSPACE: usize = 8;
/// Terminals Puddle can have open in all.
pub const MAX_IN_ALL: usize = 32;
/// The most output the host holds that the page has not acknowledged.
pub const WINDOW_BYTES: u64 = 1 << 20;
/// The most bytes one message holds, both ways.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// The sub-protocol the socket speaks.
pub const SUBPROTOCOL: &str = "puddle-terminal.v1";

/// A terminal's size in character cells, held to the bounds the screen design sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    cols: u16,
    rows: u16,
}

impl TerminalSize {
    /// The size, or `None` when it is outside 2 to 500 columns and 1 to 300 rows.
    #[must_use]
    pub fn new(cols: u32, rows: u32) -> Option<Self> {
        let cols = u16::try_from(cols).ok().filter(|c| (2..=500).contains(c))?;
        let rows = u16::try_from(rows).ok().filter(|r| (1..=300).contains(r))?;
        Some(Self { cols, rows })
    }

    /// Columns.
    #[must_use]
    pub fn cols(self) -> u16 {
        self.cols
    }

    /// Rows.
    #[must_use]
    pub fn rows(self) -> u16 {
        self.rows
    }
}

/// Why a terminal ended, as the socket's `ended` message says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// The shell exited (the status says how).
    Exited,
    /// The workspace stopped or was deleted.
    Stopped,
    /// The workspace crashed.
    Crashed,
    /// The boot hook failed, or the workspace was not ready in time.
    BootFailed,
    /// Puddle is shutting down.
    Quitting,
}

/// How a terminal ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalEnd {
    /// Why.
    pub reason: EndReason,
    /// The shell's exit status, for [`EndReason::Exited`] when it gave one.
    pub status: Option<u32>,
    /// Text for the user; it can quote the guest, so a client escapes it.
    pub message: Option<String>,
}

impl TerminalEnd {
    /// An end with no status and no message.
    #[must_use]
    pub fn of(reason: EndReason) -> Self {
        Self {
            reason,
            status: None,
            message: None,
        }
    }
}

/// What the page sends to a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    /// Bytes for the shell.
    Bytes(Vec<u8>),
    /// A new size.
    Resize(TerminalSize),
    /// The page has drawn this many more output bytes.
    Ack(u64),
}

/// What a session sends to the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalOutput {
    /// Bytes from the shell.
    Bytes(Vec<u8>),
    /// The last item: the session is over.
    Ended(TerminalEnd),
}

/// One open session. Dropping `input` ends it.
#[derive(Debug)]
pub struct TerminalLink {
    /// To the session.
    pub input: mpsc::Sender<TerminalInput>,
    /// From the session.
    pub output: mpsc::Receiver<TerminalOutput>,
}

/// Why a session could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TerminalError {
    /// Not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// The workspace is not running and not starting (409).
    #[error("{0}")]
    NotRunning(String),
    /// The session could not be reached once the socket was open; the page is told why.
    #[error("{}", .0.message.as_deref().unwrap_or("the terminal could not be opened"))]
    Ended(TerminalEnd),
}

/// What the terminal routes ask the host.
pub trait TerminalService: Send + Sync {
    /// Whether a terminal can be asked for now: the workspace is running or starting.
    fn check(&self, workspace: &WorkspaceId) -> BoxFuture<'_, Result<(), TerminalError>>;

    /// Opens a shell session in the workspace, waiting while it finishes starting.
    fn open(
        &self,
        workspace: &WorkspaceId,
        size: TerminalSize,
    ) -> BoxFuture<'_, Result<TerminalLink, TerminalError>>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTerminals;

fn unavailable() -> TerminalError {
    TerminalError::Unavailable("terminals are not available in this build".into())
}

impl TerminalService for NoTerminals {
    fn check(&self, _: &WorkspaceId) -> BoxFuture<'_, Result<(), TerminalError>> {
        Box::pin(async { Err(unavailable()) })
    }

    fn open(
        &self,
        _: &WorkspaceId,
        _: TerminalSize,
    ) -> BoxFuture<'_, Result<TerminalLink, TerminalError>> {
        Box::pin(async { Err(unavailable()) })
    }
}

/// An in-memory service for tests and the UI fixture: every workspace runs and the shell is a
/// stand-in that echoes what it is typed. `exit N` ends it with status `N`; `size` prints the
/// rows and columns it was last given; `vanish` drops the session without an end.
#[derive(Debug, Default)]
pub struct FakeTerminals {
    stopped: Mutex<Vec<WorkspaceId>>,
    refuse_next: Mutex<Option<TerminalError>>,
}

impl FakeTerminals {
    /// A fake whose workspaces all run.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `workspace` not running, so a terminal for it is refused.
    pub fn set_stopped(&self, workspace: &WorkspaceId) {
        self.stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(workspace.clone());
    }
}

impl FakeTerminals {
    /// Makes the next `open` fail with `error`.
    pub fn refuse_next_open(&self, error: TerminalError) {
        *self
            .refuse_next
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(error);
    }
}

impl TerminalService for FakeTerminals {
    fn check(&self, workspace: &WorkspaceId) -> BoxFuture<'_, Result<(), TerminalError>> {
        let stopped = self
            .stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(workspace);
        Box::pin(async move {
            if stopped {
                Err(TerminalError::NotRunning(
                    "the workspace is not running".into(),
                ))
            } else {
                Ok(())
            }
        })
    }

    fn open(
        &self,
        workspace: &WorkspaceId,
        size: TerminalSize,
    ) -> BoxFuture<'_, Result<TerminalLink, TerminalError>> {
        let checked = self.check(workspace);
        let refused = self
            .refuse_next
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        Box::pin(async move {
            checked.await?;
            if let Some(error) = refused {
                return Err(error);
            }
            Ok(fake_shell(size))
        })
    }
}

fn fake_shell(mut size: TerminalSize) -> TerminalLink {
    let (input, mut from_page) = mpsc::channel(64);
    let (to_page, output) = mpsc::channel(64);
    tokio::spawn(async move {
        let mut line = Vec::new();
        if to_page
            .send(TerminalOutput::Bytes(b"$ ".to_vec()))
            .await
            .is_err()
        {
            return;
        }
        while let Some(item) = from_page.recv().await {
            let mut answer = Vec::new();
            match item {
                TerminalInput::Resize(new) => size = new,
                TerminalInput::Ack(_) => {}
                TerminalInput::Bytes(bytes) => {
                    for byte in bytes {
                        if byte == b'\r' {
                            let text = String::from_utf8_lossy(&std::mem::take(&mut line))
                                .trim()
                                .to_owned();
                            answer.extend_from_slice(b"\r\n");
                            if let Some(status) = text.strip_prefix("exit ") {
                                let end = TerminalEnd {
                                    reason: EndReason::Exited,
                                    status: status.trim().parse().ok(),
                                    message: None,
                                };
                                // the page's receiver is gone when this fails, and the session ends with this branch
                                let _ = to_page.send(TerminalOutput::Bytes(answer)).await;
                                let _ = to_page.send(TerminalOutput::Ended(end)).await;
                                return;
                            }
                            if text == "vanish" {
                                return;
                            }
                            if text == "size" {
                                answer.extend_from_slice(
                                    format!("{} {}\r\n", size.rows(), size.cols()).as_bytes(),
                                );
                            }
                            answer.extend_from_slice(b"$ ");
                        } else {
                            line.push(byte);
                            answer.push(byte);
                        }
                    }
                }
            }
            if !answer.is_empty() && to_page.send(TerminalOutput::Bytes(answer)).await.is_err() {
                return;
            }
        }
    });
    TerminalLink { input, output }
}

/// A refusal to hand out a ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Limit {
    Workspace,
    All,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace => write!(
                f,
                "this workspace has {MAX_PER_WORKSPACE} terminals open, the most one can have; close one first"
            ),
            Self::All => write!(
                f,
                "{MAX_IN_ALL} terminals are open in all, the most Puddle can have; close one first"
            ),
        }
    }
}

struct Entry {
    workspace: WorkspaceId,
    size: TerminalSize,
    ticket: Option<(String, Instant)>,
    /// The signed-in browser the terminal was made for, if it was not made with the install token.
    device: Option<puddle_store::DeviceId>,
}

/// The tickets handed out and the terminals they open, with the limits on both. A terminal counts
/// from its ticket until its socket ends, or until its ticket runs out unused.
#[derive(Default)]
pub(crate) struct Registry {
    inner: Mutex<RegistryInner>,
}

#[derive(Default)]
struct RegistryInner {
    next: u64,
    entries: BTreeMap<u64, Entry>,
}

impl Registry {
    /// Makes a terminal and its ticket (64 hex characters).
    pub(crate) fn create(
        &self,
        workspace: &WorkspaceId,
        size: TerminalSize,
        ticket: String,
    ) -> Result<u64, Limit> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        inner.entries.retain(|_, e| match &e.ticket {
            Some((_, issued)) => now.duration_since(*issued) < TICKET_LIFETIME,
            None => true,
        });
        if inner.entries.len() >= MAX_IN_ALL {
            return Err(Limit::All);
        }
        if inner
            .entries
            .values()
            .filter(|e| &e.workspace == workspace)
            .count()
            >= MAX_PER_WORKSPACE
        {
            return Err(Limit::Workspace);
        }
        inner.next += 1;
        let id = inner.next;
        inner.entries.insert(
            id,
            Entry {
                workspace: workspace.clone(),
                size,
                ticket: Some((ticket, now)),
                device: None,
            },
        );
        Ok(id)
    }

    /// Records that terminal `id` belongs to the signed-in browser `device`, so it ends when that
    /// device is signed out.
    pub(crate) fn bind(&self, id: u64, device: puddle_store::DeviceId) {
        if let Some(entry) = self
            .inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get_mut(&id)
        {
            entry.device = Some(device);
        }
    }

    /// The signed-in browser terminal `id` was made for.
    pub(crate) fn device_of(&self, id: u64) -> Option<puddle_store::DeviceId> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .get(&id)
            .and_then(|entry| entry.device)
    }

    /// Uses the ticket of terminal `id` of `workspace`: works once, within its lifetime, for that
    /// terminal only. A wrong guess leaves the ticket as it was.
    pub(crate) fn claim(
        &self,
        workspace: &WorkspaceId,
        id: u64,
        presented: &[u8],
    ) -> Option<TerminalSize> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let entry = inner.entries.get_mut(&id)?;
        if &entry.workspace != workspace {
            return None;
        }
        let (ticket, issued) = entry.ticket.as_ref()?;
        let fresh = Instant::now().duration_since(*issued) < TICKET_LIFETIME;
        if !fresh || !bool::from(ticket.as_bytes().ct_eq(presented)) {
            return None;
        }
        entry.ticket = None;
        Some(entry.size)
    }

    /// Frees the terminal's place.
    pub(crate) fn release(&self, id: u64) {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .remove(&id);
    }
}

#[cfg(test)]
mod tests;
