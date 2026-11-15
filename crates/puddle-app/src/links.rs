// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the user is told when a link in the window goes nowhere.
//!
//! A clicked link either opens in the default browser or it doesn't: the opener can fail, and the
//! window refuses anything that isn't a plain web address. Neither shows anything on its own, and
//! a desktop user never sees stderr, so both are raised as a background problem. The page already
//! lists those as notices (`GET /api/problems`), so this needs no window API and no UI of its own.
//!
//! A window the editor asked for ("New Window") that could not be built is a problem of its own
//! per workspace: the link was fine, so naming its scheme would mislead.
//!
//! One problem key serves the two link cases: the newest failure replaces the one before, and the
//! next link that opens takes it away. A window's problem goes when a window of that workspace
//! next opens.

use std::fmt::Display;

use puddle_types::{Problem, Problems, WorkspaceId};
use url::Url;

/// The key of the problem about a link that went nowhere.
const PROBLEM_KEY: &str = "app:link";

/// The start of the key of a workspace's problem about a window that could not be built.
const WINDOW_KEY: &str = "app:window";

/// The problem for a web address the default browser could not be asked to open. Only the host is
/// named: an address can carry a token in its query.
#[must_use]
pub fn open_failed(url: &Url, reason: &str) -> Problem {
    Problem::new(
        PROBLEM_KEY,
        format!(
            "Puddle could not open a link in your browser ({})",
            where_to(url)
        ),
        format!(
            "{reason}. Nothing was opened. Check that a default web browser is set, then click \
             the link again."
        ),
    )
}

/// The problem for a link the window refuses to follow. Only the scheme is named: the rest of an
/// address is the page's to show.
#[must_use]
pub fn refused(url: &Url) -> Problem {
    Problem::new(
        PROBLEM_KEY,
        format!("Puddle did not follow a link ({}:)", url.scheme()),
        "Puddle only opens web addresses (http or https) from this window, so nothing was opened.",
    )
}

/// The problem for an editor window ("New Window") that could not be built. It names the
/// workspace and the reason, not the address: the link was the editor's own and fine.
#[must_use]
pub fn window_failed(workspace: &WorkspaceId, reason: &str) -> Problem {
    Problem::new(
        format!("{WINDOW_KEY}:{workspace}"),
        format!("Puddle could not open another editor window for {workspace}"),
        format!("{reason}. The window you already have still works; try New Window again."),
    )
    .about(workspace.clone())
}

/// Withdraws the notice about `workspace`'s failed window, once one opened.
pub fn window_opened(problems: &Problems, workspace: &WorkspaceId) {
    problems.clear(&format!("{WINDOW_KEY}:{workspace}"));
}

/// Raises the notice for a window that could not be built.
pub fn report_window_failed(problems: &Problems, workspace: &WorkspaceId, reason: &str) {
    let problem = window_failed(workspace, reason);
    problems.clear(&problem.key);
    problems.raise(problem);
}

/// The host of `url`, or its scheme when it has none.
fn where_to(url: &Url) -> String {
    url.host_str()
        .map_or_else(|| format!("{}:", url.scheme()), str::to_owned)
}

/// Raises the notice for a refused link.
pub fn report_refused(problems: &Problems, url: &Url) {
    raise_fresh(problems, refused(url));
}

/// Opens `url` with `open` and tells the user when that fails; a success withdraws an earlier
/// notice.
pub fn open_and_report<E: Display>(
    problems: &Problems,
    url: &Url,
    open: impl FnOnce(&str) -> Result<(), E>,
) {
    match open(url.as_str()) {
        Ok(()) => problems.clear(PROBLEM_KEY),
        Err(err) => raise_fresh(problems, open_failed(url, &err.to_string())),
    }
}

/// Withdraws the problem before raising it, so a page that read the list in between sees it go and
/// come back. A page keeps a notice the user dismissed dismissed while the problem reads the same.
fn raise_fresh(problems: &Problems, problem: Problem) {
    problems.clear(PROBLEM_KEY);
    problems.raise(problem);
}

#[cfg(test)]
mod tests;
