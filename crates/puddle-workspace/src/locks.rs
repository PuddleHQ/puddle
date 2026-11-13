// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stale git lock files after a crash: `guest/clear-locks.sh` run after a boot, and its
//! parsed result.
//!
//! A VMM kill leaves `index.lock`, `HEAD.lock`, `refs/**.lock` and friends behind, and git then
//! refuses to work ("Another git process seems to be running"). Right after a boot no git
//! process of the user's exists, so a lock is stale; the script still checks for a running `git`
//! and removes nothing if it finds one. The same script then repairs a zero-filled `HEAD` and a
//! zero-filled or corrupt index (what a host power cut leaves) and reports what it did. Its output is parsed as hostile input: bounded, unknown
//! records refused, end marker required.

use std::time::Duration;

use puddle_compute::{ExecRequest, Sandbox};
use puddle_types::WorkspaceId;

use crate::{Layout, WorkspaceError};

/// The script (POSIX sh, run as root with the mount point as `$1`).
pub const CLEAR_LOCKS_SH: &str = include_str!("../guest/clear-locks.sh");

/// How long the script may take.
pub const CLEAR_LOCKS_TIMEOUT: Duration = Duration::from_secs(60);

/// Most removed paths kept in a [`LockReport`]; the rest are counted in [`LockReport::more`].
pub const MAX_LOCKS: usize = 100;

/// Longest path or message kept.
const MAX_LINE: usize = 500;

/// What clearing the stale locks of one workspace did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LockReport {
    /// A git process was running, so nothing was removed.
    pub skipped_busy: bool,
    /// Lock files removed, relative to the volume root (at most [`MAX_LOCKS`]).
    pub removed: Vec<String>,
    /// How many more were removed than listed.
    pub more: u64,
    /// Locks (or checkouts) that could not be handled, as `path: why`.
    pub errors: Vec<String>,
    /// Zero-filled or corrupt git files that were repaired, as `path: what was done` (at most
    /// [`MAX_LOCKS`]).
    pub repaired: Vec<String>,
    /// Damaged git files that were found and left alone, as `path: why` (at most [`MAX_LOCKS`]).
    pub damaged: Vec<String>,
}

impl LockReport {
    /// How many lock files were removed.
    #[must_use]
    pub fn removed_count(&self) -> u64 {
        self.removed.len() as u64 + self.more
    }
}

/// The command: the script with the mount point, as root.
pub(crate) fn request(layout: &Layout) -> ExecRequest {
    ExecRequest::new(
        "sh",
        [
            "-c",
            CLEAR_LOCKS_SH,
            "puddle-clear-locks",
            layout.mount().as_str(),
        ]
        .map(str::to_owned),
    )
    .as_user("root")
    .with_timeout(CLEAR_LOCKS_TIMEOUT)
}

/// Runs the script for `id` in `sandbox`.
pub(crate) async fn run<S: Sandbox>(
    sandbox: &S,
    id: &WorkspaceId,
) -> Result<LockReport, WorkspaceError> {
    let layout = Layout::new(id)?;
    let out = sandbox
        .exec(request(&layout))
        .await
        .map_err(|e| WorkspaceError::runtime("clear stale git locks", id, e))?;
    let fail = |reason: String| WorkspaceError::Locks {
        workspace: id.to_string(),
        reason,
    };
    if !out.status.success() {
        // The script's own error line when it wrote one, else what the shell said.
        let reason = out
            .stdout_text()
            .lines()
            .find_map(|l| l.strip_prefix("E\t"))
            .map(|l| l.replace('\t', ": "))
            .filter(|l| !l.is_empty())
            .unwrap_or_else(|| crate::trim::tail(&out.stderr_text()));
        return Err(fail(format!("exited {}: {reason}", out.status.code)));
    }
    parse(&out.stdout_text()).map_err(fail)
}

/// Adds `path: note` to `list` unless it is full.
fn push_bounded(list: &mut Vec<String>, path: &str, note: &str) {
    if list.len() < MAX_LOCKS {
        list.push(format!("{path}: {note}").chars().take(MAX_LINE).collect());
    }
}

/// Parses the script's output.
pub(crate) fn parse(stdout: &str) -> Result<LockReport, String> {
    let mut report = LockReport::default();
    let mut done = false;
    for line in stdout.lines() {
        if done {
            return Err("output after the end marker".into());
        }
        let mut fields = line.splitn(3, '\t');
        let tag = fields.next().unwrap_or_default();
        let a = fields.next();
        let b = fields.next();
        match (tag, a, b) {
            ("D", None, None) => done = true,
            ("B", None, None) => report.skipped_busy = true,
            ("L", Some(path), None) => {
                if report.removed.len() < MAX_LOCKS {
                    report.removed.push(path.chars().take(MAX_LINE).collect());
                } else {
                    report.more = report.more.saturating_add(1);
                }
            }
            ("E", Some(path), Some(why)) => {
                if report.errors.len() < MAX_LOCKS {
                    report
                        .errors
                        .push(format!("{path}: {why}").chars().take(MAX_LINE).collect());
                }
            }
            ("R", Some(path), Some(what)) => push_bounded(&mut report.repaired, path, what),
            ("P", Some(path), Some(why)) => push_bounded(&mut report.damaged, path, why),
            _ => {
                let shown: String = line.chars().take(80).collect();
                return Err(format!("unexpected output line {shown:?}"));
            }
        }
    }
    if done {
        Ok(report)
    } else {
        Err("the script did not finish (no end marker)".into())
    }
}

#[cfg(test)]
mod tests;
