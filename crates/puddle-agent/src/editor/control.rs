// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor's update control: the schedule, the builds folder and the locks that keep
//! downloads, switches and clean-ups apart.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use puddle_agent_proto::editor::{EditorServer, ServerKind};
use tokio::sync::{Mutex, Notify, watch};

use super::end;
use crate::microsoft;

/// When the agent looks for a newer build of Microsoft's server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTimes {
    /// The wait before the first look, so a server that is starting is not slowed by it.
    pub first: Duration,
    /// The wait between two looks that worked.
    pub every: Duration,
    /// The wait before looking again after a look that failed (no network, a refused download).
    pub retry: Duration,
}

impl Default for UpdateTimes {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(60),
            every: Duration::from_hours(24),
            retry: Duration::from_mins(15),
        }
    }
}

/// What the host's messages and the update schedule share with the running server: the builds
/// folder, the lock that keeps downloads and clean-ups apart, and the request to restart.
#[derive(Debug)]
pub struct Control {
    pub(super) builds: PathBuf,
    /// Held by an install, a staging download and a clean-up: a clean-up during a download would
    /// remove the download's folders.
    pub(super) work: Mutex<()>,
    /// Set by a rollback that could not clean up because a download held `work`: the holder
    /// cleans up before it lets go, so the failed build's folder does not stay until some later
    /// confirmation.
    pub(super) cleanup_owed: std::sync::atomic::AtomicBool,
    pub(super) restart: Notify,
    /// Held by whoever reads or moves the pointers (a switch, a claim of the build to run, a
    /// confirmation, a rollback), never across an await: those run in a few microseconds and must
    /// not interleave, or a build that is running could be deleted as "the previous one".
    pub(super) pointers: std::sync::Mutex<()>,
    pub(super) times: UpdateTimes,
    /// Whether this server updates at all (Microsoft's, with the user's setting on).
    pub(super) updates: bool,
    /// Why the server cannot be installed, `None` when it can: every refused or failed download
    /// sets it again (also to the same text), so a host that missed one hears the next.
    pub(super) install_problem: watch::Sender<Option<String>>,
    /// Why a newer build could not be taken, `None` when nothing is wrong; set again in the same
    /// way. The server runs on the build it has meanwhile.
    pub(super) update_problem: watch::Sender<Option<String>>,
    /// Why the server runs as root though a session user was named, `None` when it runs as the
    /// user it should; set again at each start that lacks the user.
    pub(super) session_user_problem: watch::Sender<Option<String>>,
}

impl Control {
    /// The control of the server `server` runs.
    #[must_use]
    pub fn new(server: &EditorServer) -> Arc<Self> {
        Self::with_times(server, UpdateTimes::default())
    }

    /// [`Control::new`] with other waits (tests).
    #[must_use]
    pub fn with_times(server: &EditorServer, times: UpdateTimes) -> Arc<Self> {
        Arc::new(Self {
            builds: PathBuf::from(&server.dir),
            work: Mutex::new(()),
            cleanup_owed: std::sync::atomic::AtomicBool::new(false),
            restart: Notify::new(),
            pointers: std::sync::Mutex::new(()),
            times,
            install_problem: watch::channel(None).0,
            update_problem: watch::channel(None).0,
            session_user_problem: watch::channel(None).0,
            updates: matches!(
                server.kind,
                ServerKind::Microsoft {
                    auto_update: true,
                    ..
                }
            ),
        })
    }

    /// Runs the staged build, if there is one, by restarting the server: the host calls this when
    /// no window is connected. Returns the commit now current, `None` when nothing was staged.
    ///
    /// # Errors
    ///
    /// When the switch could not be recorded; the old build keeps running.
    pub fn switch(&self) -> io::Result<Option<String>> {
        if !self.updates {
            return Ok(None);
        }
        let _pointers = self.pointers_lock();
        let switched = microsoft::switch_to_staged(&self.builds)?;
        if switched.is_some() {
            self.restart.notify_one();
        }
        Ok(switched)
    }
}

impl Control {
    /// Follows why the server cannot be installed ([`forward_editor_problems`] sends it to the
    /// host). Subscribe before the server runs: a receiver sees only what is set after it exists.
    #[must_use]
    pub fn install_problem(&self) -> watch::Receiver<Option<String>> {
        self.install_problem.subscribe()
    }

    /// Follows why a newer build could not be taken, like [`Control::install_problem`].
    #[must_use]
    pub fn update_problem(&self) -> watch::Receiver<Option<String>> {
        self.update_problem.subscribe()
    }

    /// Follows why the server runs as root though a session user was named, like
    /// [`Control::install_problem`].
    #[must_use]
    pub fn session_user_problem(&self) -> watch::Receiver<Option<String>> {
        self.session_user_problem.subscribe()
    }

    /// A start found no session user in the guest: say why, every time.
    pub(super) fn session_user_missing(&self, reason: &str) {
        self.session_user_problem
            .send_replace(Some(reason.to_owned()));
    }

    /// A start found the session user, or none was named: ends what
    /// [`Control::session_user_missing`] said, once.
    pub(super) fn session_user_found(&self) {
        end(&self.session_user_problem);
    }

    /// A download was refused, or failed on the network again and again: say why, every time.
    pub(super) fn install_failed(&self, reason: &str) {
        self.install_problem.send_replace(Some(reason.to_owned()));
    }

    /// The server got past its install: ends what [`Control::install_failed`] said, once.
    pub(super) fn install_worked(&self) {
        end(&self.install_problem);
    }

    /// A newer build was refused, started badly or could not be looked for: say why, every time.
    pub(super) fn update_failed(&self, reason: &str) {
        self.update_problem.send_replace(Some(reason.to_owned()));
    }

    /// A look for a newer build worked: ends what [`Control::update_failed`] said, once.
    pub(super) fn update_worked(&self) {
        end(&self.update_problem);
    }

    pub(super) fn pointers_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        // Nothing under the lock panics; a poisoned lock is as good as a free one.
        self.pointers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The build to run now and whether it is on trial, read together with the restart request
    /// they answer: a switch before this is the build returned, one after it restarts the server.
    pub(super) fn claim(&self) -> Option<(PathBuf, bool)> {
        let _pointers = self.pointers_lock();
        // A request left over from before is answered by this very read.
        let _ = self.restart.notified().now_or_never();
        let build = microsoft::current_build(&self.builds)?;
        Some((build, microsoft::on_trial(&self.builds)))
    }
}
