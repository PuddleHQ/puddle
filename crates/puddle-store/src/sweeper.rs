// SPDX-License-Identifier: AGPL-3.0-or-later
//! The background sweeper (`docs/spec/rules.md` §4): one pass at start, then one per period.

use std::sync::Arc;
use std::time::Duration;

use puddle_types::{NullSink, Problem, Problems};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::store::Store;

/// How often the sweeper runs by default (R-19).
pub const DEFAULT_SWEEP_PERIOD: Duration = Duration::from_secs(60);

/// The key of the problem a failed sweep raises.
pub const SWEEP_PROBLEM: &str = "sweeper";

/// The key of the problem raised while the activity log is over its warning size.
pub const AUDIT_SIZE_PROBLEM: &str = "audit-size";

fn audit_size_problem(bytes: u64, retention_days: u32) -> Problem {
    Problem::new(
        AUDIT_SIZE_PROBLEM,
        "The activity log is getting large",
        format!(
            "It holds about {} MiB, over the size Puddle warns at. Nothing is deleted for its size: \
             records are kept for {retention_days} days. If the disk is getting full, shorten that \
             in the admin screen, or prune it by hand with the `audit prune` command.",
            bytes / (1024 * 1024)
        ),
    )
}

fn sweep_problem(reason: &str) -> Problem {
    Problem::new(
        SWEEP_PROBLEM,
        "Puddle could not tidy up its rules and activity log",
        format!(
            "{reason}. Rules that expired keep applying, and the activity log is not trimmed or \
             completed, until it works; Puddle tries again every minute. A full disk is the usual \
             cause: free some space."
        ),
    )
}

/// Raises [`AUDIT_SIZE_PROBLEM`] while the audit is over its warning size, and ends it after.
fn warn_when_large(store: &Store, problems: &Problems, over: bool) {
    if !over {
        problems.clear(AUDIT_SIZE_PROBLEM);
        return;
    }
    // A read that fails is not worth a second problem; the next pass looks again.
    if let (Ok(bytes), Ok(days)) = (store.audit_bytes(), store.audit_retention_days()) {
        problems.raise(audit_size_problem(bytes, days));
    }
}

/// A running sweeper. Its owner ends it with [`Sweeper::shutdown`]; dropping the handle also
/// stops it after the current pass.
#[derive(Debug)]
pub struct Sweeper {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Sweeper {
    /// Starts sweeping `store` every `period` on the current tokio runtime. Each pass runs on a
    /// blocking thread; a failed pass is logged and retried next period.
    #[must_use]
    pub fn spawn(store: Arc<Store>, period: Duration) -> Self {
        Self::spawn_reporting(store, period, Arc::new(Problems::new(Arc::new(NullSink))))
    }

    /// Like [`Sweeper::spawn`], and a failed pass raises the problem [`SWEEP_PROBLEM`] in
    /// `problems`, which the next pass that works ends.
    #[must_use]
    pub fn spawn_reporting(store: Arc<Store>, period: Duration, problems: Arc<Problems>) -> Self {
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                let pass = Arc::clone(&store);
                // A pass that failed and a pass that panicked are the same to the user.
                let pass = tokio::task::spawn_blocking(move || pass.sweep())
                    .await
                    .map_err(|err| err.to_string())
                    .and_then(|swept| swept.map_err(|err| err.to_string()));
                match pass {
                    Ok(report) => {
                        tracing::debug!(?report, "sweep done");
                        problems.clear(SWEEP_PROBLEM);
                        warn_when_large(&store, &problems, report.audit_over_warn_size);
                    }
                    Err(reason) => problems.raise(sweep_problem(&reason)),
                }
                tokio::select! {
                    () = tokio::time::sleep(period) => {}
                    _ = &mut stopped => break,
                }
            }
        });
        Self { stop, task }
    }

    /// Stops the sweeper and waits for its current pass to finish.
    pub async fn shutdown(self) {
        // The task may already have ended; then there is no one to tell.
        let _ = self.stop.send(());
        if let Err(err) = self.task.await {
            tracing::warn!(%err, "sweeper ended abnormally");
        }
    }
}

#[cfg(test)]
mod tests;
