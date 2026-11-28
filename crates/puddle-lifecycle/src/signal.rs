// SPDX-License-Identifier: AGPL-3.0-or-later
//! What tells Puddle to shut down.

use std::io::{BufRead, BufReader, Read, Write};

use tokio::sync::oneshot;

/// Why Puddle is shutting down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShutdownCause {
    /// Ctrl-C, or `SIGINT`.
    Interrupt,
    /// Ctrl-Break.
    Break,
    /// `SIGTERM`.
    Terminate,
    /// `SIGHUP` (the terminal went away).
    Hangup,
    /// The console window was closed. Windows ends the front process about 5 s later; the worker
    /// finishes the shutdown on its own (see [`crate::supervise`]).
    ConsoleClose,
    /// The user is logging off.
    Logoff,
    /// The system is shutting down.
    SystemShutdown,
    /// The worker's control pipe closed without a request: the front process is gone.
    FrontGone,
}

impl ShutdownCause {
    const ALL: [Self; 8] = [
        Self::Interrupt,
        Self::Break,
        Self::Terminate,
        Self::Hangup,
        Self::ConsoleClose,
        Self::Logoff,
        Self::SystemShutdown,
        Self::FrontGone,
    ];

    /// A short name for logs and the front-to-worker control line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupt => "interrupt",
            Self::Break => "break",
            Self::Terminate => "terminate",
            Self::Hangup => "hangup",
            Self::ConsoleClose => "console-close",
            Self::Logoff => "logoff",
            Self::SystemShutdown => "system-shutdown",
            Self::FrontGone => "front-gone",
        }
    }

    /// Whether Windows ends the process shortly after this event, whatever it does: the
    /// shutdown has seconds, not minutes.
    #[must_use]
    pub fn is_deadline(self) -> bool {
        matches!(
            self,
            Self::ConsoleClose | Self::Logoff | Self::SystemShutdown
        )
    }

    fn parse(line: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == line)
    }
}

impl std::fmt::Display for ShutdownCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The front's request on the worker's control pipe: one line naming the cause. EOF, an unknown
/// line or a read error all mean the front is gone, which is also a reason to shut down.
fn read_request<R: Read>(control: R) -> ShutdownCause {
    let mut line = String::new();
    match BufReader::new(control).read_line(&mut line) {
        Ok(n) if n > 0 => ShutdownCause::parse(line.trim_end()).unwrap_or(ShutdownCause::FrontGone),
        _ => ShutdownCause::FrontGone,
    }
}

/// The front's side of the control pipe: passes a console event on to the worker, `control`
/// being the pipe while it is still open. The first event writes the cause and closes the pipe
/// (closing is the request too, so a failed write changes nothing); later ones find it closed.
/// Returns whether the default handler should end the front: a repeated interrupt or break once
/// the shutdown was requested (a hung shutdown can be cut short). A deadline event (the console
/// closes, the user logs off, the system shuts down) never does: the front must stay to wait.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the front exists on Windows only")
)]
pub(crate) fn request_worker_shutdown<W: Write>(control: Option<W>, cause: ShutdownCause) -> bool {
    let requested_before = control.is_none();
    if let Some(mut control) = control {
        let _ = writeln!(control, "{cause}");
        drop(control);
        tracing::info!(%cause, "front: shutdown requested");
    }
    requested_before && !cause.is_deadline()
}

/// Reads [`read_request`] on a thread of its own, started now. A blocking read on the runtime's
/// pool would keep the process alive after the shutdown: the front's pipe stays open until the
/// front ends, so the read stays parked, and the runtime waits for pool threads when it drops.
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "the worker reads it on Windows only")
)]
pub(crate) fn spawn_request_reader<R: Read + Send + 'static>(
    control: R,
) -> std::io::Result<oneshot::Receiver<ShutdownCause>> {
    let (sender, receiver) = oneshot::channel();
    std::thread::Builder::new()
        .name("shutdown-request".into())
        .spawn(move || {
            // a dropped receiver means nobody waits for the request any more
            let _ = sender.send(read_request(control));
        })?;
    Ok(receiver)
}

/// The shutdown triggers, installed. Install them early (a `SIGTERM` that arrives before
/// installation ends the process the default way), inside a tokio runtime.
///
/// - Windows worker (under [`crate::supervise`]): the front's request on stdin, or the front
///   going away.
/// - Windows otherwise: Ctrl-C, Ctrl-Break, console close, logoff, system shutdown. A console
///   close leaves this process about 5 s, too little to stop the VMs; use [`crate::supervise`].
/// - Unix: `SIGINT`, `SIGTERM`, `SIGHUP`.
#[derive(Debug)]
pub struct ShutdownSignals(imp::Signals);

impl ShutdownSignals {
    /// Installs the handlers.
    ///
    /// # Errors
    ///
    /// When a signal handler can't be installed.
    pub fn install() -> std::io::Result<Self> {
        imp::Signals::install().map(Self)
    }

    /// Resolves at the first trigger.
    pub async fn recv(&mut self) -> ShutdownCause {
        self.0.recv().await
    }

    /// Runs `work`, the shutdown that [`Self::recv`]'s trigger started, until it ends or the next
    /// trigger arrives, whichever is first. `Err` carries that second trigger: the caller gives up
    /// waiting for the stop to finish instead of leaving a stuck shutdown to `SIGKILL`.
    ///
    /// The Windows worker has no second trigger (the front's one request is its only input),
    /// so `work` always runs to its end there.
    ///
    /// # Errors
    ///
    /// The cause of the second trigger, when it came before `work` ended.
    pub async fn until_again<F: Future>(&mut self, work: F) -> Result<F::Output, ShutdownCause> {
        tokio::select! {
            out = work => Ok(out),
            cause = self.0.recv_again() => Err(cause),
        }
    }
}

/// [`ShutdownSignals::install`], then [`ShutdownSignals::recv`].
///
/// # Errors
///
/// When a signal handler can't be installed.
pub async fn wait_for_shutdown() -> std::io::Result<ShutdownCause> {
    Ok(ShutdownSignals::install()?.recv().await)
}

#[cfg(unix)]
mod imp {
    use tokio::signal::unix::{Signal, SignalKind, signal};

    use super::ShutdownCause;

    #[derive(Debug)]
    pub(super) struct Signals {
        int: Signal,
        term: Signal,
        hup: Signal,
    }

    impl Signals {
        pub(super) fn install() -> std::io::Result<Self> {
            Ok(Self {
                int: signal(SignalKind::interrupt())?,
                term: signal(SignalKind::terminate())?,
                hup: signal(SignalKind::hangup())?,
            })
        }

        pub(super) async fn recv(&mut self) -> ShutdownCause {
            tokio::select! {
                _ = self.int.recv() => ShutdownCause::Interrupt,
                _ = self.term.recv() => ShutdownCause::Terminate,
                _ = self.hup.recv() => ShutdownCause::Hangup,
            }
        }

        pub(super) async fn recv_again(&mut self) -> ShutdownCause {
            self.recv().await
        }
    }
}

#[cfg(windows)]
mod imp {
    use tokio::signal::windows::{
        CtrlBreak, CtrlC, CtrlClose, CtrlLogoff, CtrlShutdown, ctrl_break, ctrl_c, ctrl_close,
        ctrl_logoff, ctrl_shutdown,
    };

    use super::{ShutdownCause, spawn_request_reader};

    #[derive(Debug)]
    pub(super) enum Signals {
        /// Under the front: its request arrives on stdin, read by a thread started at install.
        Worker(Option<tokio::sync::oneshot::Receiver<ShutdownCause>>),
        Console {
            c: CtrlC,
            brk: CtrlBreak,
            close: CtrlClose,
            logoff: CtrlLogoff,
            shutdown: CtrlShutdown,
        },
    }

    impl Signals {
        pub(super) fn install() -> std::io::Result<Self> {
            if crate::supervise::is_worker() {
                return Ok(Self::Worker(Some(spawn_request_reader(std::io::stdin())?)));
            }
            Ok(Self::Console {
                c: ctrl_c()?,
                brk: ctrl_break()?,
                close: ctrl_close()?,
                logoff: ctrl_logoff()?,
                shutdown: ctrl_shutdown()?,
            })
        }

        pub(super) async fn recv(&mut self) -> ShutdownCause {
            match self {
                Self::Worker(request) => match request.take() {
                    // a reader thread that ended without a request is the front gone too
                    Some(request) => request.await.unwrap_or(ShutdownCause::FrontGone),
                    // The pipe was read to its end by an earlier call.
                    None => ShutdownCause::FrontGone,
                },
                Self::Console {
                    c,
                    brk,
                    close,
                    logoff,
                    shutdown,
                } => tokio::select! {
                    _ = c.recv() => ShutdownCause::Interrupt,
                    _ = brk.recv() => ShutdownCause::Break,
                    _ = close.recv() => ShutdownCause::ConsoleClose,
                    _ = logoff.recv() => ShutdownCause::Logoff,
                    _ = shutdown.recv() => ShutdownCause::SystemShutdown,
                },
            }
        }

        pub(super) async fn recv_again(&mut self) -> ShutdownCause {
            match self {
                Self::Worker(_) => std::future::pending().await,
                Self::Console { .. } => self.recv().await,
            }
        }
    }
}

#[cfg(test)]
mod tests;
