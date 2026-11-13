// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checkouts on a workspace's volume: `guest/checkouts.sh` run in a sandbox that has the
//! workspace, and its parsed result.
//!
//! The listing is guest input and is parsed as hostile: known record kinds only, an end marker
//! required, one exact shape per record, every field cut at [`MAX_FIELD`] characters with control
//! characters replaced, and a user name or password in a remote address removed before the
//! address leaves the parser (the listing says one was there). A report that does not parse is
//! refused whole, never shown in part.

use std::time::Duration;

use puddle_compute::{ExecRequest, Sandbox};
use puddle_types::{CheckoutDir, WorkspaceId};

use crate::{Layout, WorkspaceError};

/// The listing script (POSIX sh, run as root with the mount point, and optionally one directory
/// under it, as arguments).
pub const CHECKOUTS_SH: &str = include_str!("../guest/checkouts.sh");

/// How long the listing may take.
pub const CHECKOUTS_TIMEOUT: Duration = Duration::from_secs(10);

/// Most checkouts listed; the rest are counted in [`CheckoutList::more`]. The same limit as the
/// delete check's.
pub const MAX_CHECKOUTS: usize = crate::MAX_REPOS;

/// Longest field kept, in characters.
pub const MAX_FIELD: usize = 500;

/// Most unreadable checkouts kept.
const MAX_ERRORS: usize = 100;

/// Why a checkout's remote address is not offered as it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteNote {
    /// The address had a user name or password; it was removed.
    CredentialsRemoved,
    /// An SSH address.
    Ssh,
    /// Not an HTTPS address (`http://`, `git://`, a path).
    NotHttps,
    /// No remote, or several and none called `origin`.
    NoRemote,
}

/// One checkout the listing found. Every string came from the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundCheckout {
    /// The directory under the volume root.
    pub dir: String,
    /// The address of `origin` (else of the only remote) when it is an HTTPS address, with any
    /// user name or password removed.
    pub remote_url: Option<String>,
    /// Why there is no `remote_url`, or that credentials were removed from it.
    pub remote_note: Option<RemoteNote>,
    /// The branch; `None` when HEAD is detached.
    pub branch: Option<String>,
    /// The short commit; `None` before the first commit.
    pub head: Option<String>,
    /// Uncommitted changes or untracked files.
    pub dirty: bool,
    /// Why Puddle could not read it, when it could not.
    pub unreadable: Option<String>,
}

/// What the listing found on a volume.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckoutList {
    /// The first [`MAX_CHECKOUTS`] checkouts, in directory order.
    pub checkouts: Vec<FoundCheckout>,
    /// How many more there are.
    pub more: u64,
    /// How many other entries (loose files, folders that are no checkout) sit directly under the
    /// volume root; Puddle's own folder and `lost+found` are not counted. Zero for the listing of
    /// one directory.
    pub others: u64,
}

/// The command for `layout`: all checkouts, or just `only`.
pub(crate) fn request(layout: &Layout, only: Option<&CheckoutDir>) -> ExecRequest {
    let mut args = vec![
        "-c".to_owned(),
        CHECKOUTS_SH.to_owned(),
        "puddle-checkouts".to_owned(),
        layout.mount().as_str().to_owned(),
    ];
    args.extend(only.map(|dir| dir.as_str().to_owned()));
    ExecRequest::new("sh", args)
        .as_user("root")
        .with_timeout(CHECKOUTS_TIMEOUT)
}

/// Runs the listing for `id` in `sandbox`.
pub(crate) async fn run<S: Sandbox>(
    sandbox: &S,
    id: &WorkspaceId,
    only: Option<&CheckoutDir>,
) -> Result<CheckoutList, WorkspaceError> {
    let layout = Layout::new(id)?;
    let out = sandbox
        .exec(request(&layout, only))
        .await
        .map_err(|e| WorkspaceError::runtime("list the checkouts", id, e))?;
    let fail = |reason: String| WorkspaceError::Checkouts {
        workspace: id.to_string(),
        reason,
    };
    if !out.status.success() {
        let stdout = out.stdout_text();
        let first_error = stdout
            .lines()
            .find_map(|l| l.strip_prefix("E\t"))
            .unwrap_or_default();
        return Err(fail(format!(
            "the listing exited {}: {}{}",
            out.status.code,
            clean(&first_error.replace('\t', ": ")),
            clean(out.stderr_text().trim())
        )));
    }
    parse(&out.stdout_text()).map_err(fail)
}

/// A guest string made safe to keep and show: control characters replaced, cut at [`MAX_FIELD`].
fn clean(text: &str) -> String {
    text.chars()
        .take(MAX_FIELD)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| clean(text))
}

/// `scheme://user:pass@host/x` as `scheme://host/x`, and whether anything was removed.
fn without_credentials(url: &str) -> (String, bool) {
    let Some((scheme, rest)) = url.split_once("://") else {
        return (url.to_owned(), false);
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest.get(..end).and_then(|authority| authority.rfind('@')) {
        Some(at) => (
            format!("{scheme}://{}", rest.get(at + 1..).unwrap_or_default()),
            true,
        ),
        None => (url.to_owned(), false),
    }
}

/// `git@host:owner/repo`-style remotes: no scheme, a `:` before any `/`.
fn is_scp_like(url: &str) -> bool {
    if url.contains("://") {
        return false;
    }
    match (url.find(':'), url.find('/')) {
        // A colon at index 1 is a drive letter, not a host.
        (Some(colon), Some(slash)) => colon > 1 && colon < slash,
        (Some(colon), None) => colon > 1,
        _ => false,
    }
}

/// What the address of a remote makes of it: the address to show and why not, if not.
fn judge_remote(raw: &str) -> (Option<String>, Option<RemoteNote>) {
    let raw = raw.trim();
    if raw.is_empty() {
        return (None, Some(RemoteNote::NoRemote));
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("ssh://") || is_scp_like(raw) {
        return (None, Some(RemoteNote::Ssh));
    }
    if !lower.starts_with("https://") {
        return (None, Some(RemoteNote::NotHttps));
    }
    let (url, removed) = without_credentials(raw);
    (Some(url), removed.then_some(RemoteNote::CredentialsRemoved))
}

/// Parses the script's output (see `guest/checkouts.sh`).
pub(crate) fn parse(stdout: &str) -> Result<CheckoutList, String> {
    let mut list = CheckoutList::default();
    let mut errors = 0_usize;
    let mut done = false;
    for line in stdout.lines() {
        if done {
            return Err("output after the end marker".into());
        }
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["D"] => done = true,
            ["C", dir, remote, branch, head, dirty @ ("0" | "1")] if !dir.is_empty() => {
                if list.checkouts.len() == MAX_CHECKOUTS {
                    return Err(format!("more than {MAX_CHECKOUTS} checkouts listed"));
                }
                let (remote_url, remote_note) = judge_remote(&clean(remote));
                list.checkouts.push(FoundCheckout {
                    dir: clean(dir),
                    remote_url,
                    remote_note,
                    branch: non_empty(branch),
                    head: non_empty(head),
                    dirty: *dirty == "1",
                    unreadable: None,
                });
            }
            ["M", count] => {
                let n: u64 = count
                    .parse()
                    .map_err(|_| format!("bad count line {:?}", truncate(line)))?;
                list.more = list.more.saturating_add(n);
            }
            ["O", count] => {
                let n: u64 = count
                    .parse()
                    .map_err(|_| format!("bad count line {:?}", truncate(line)))?;
                list.others = list.others.saturating_add(n);
            }
            ["E", dir, message] if !dir.is_empty() => {
                errors += 1;
                if errors > MAX_ERRORS {
                    continue;
                }
                list.checkouts.push(FoundCheckout {
                    dir: clean(dir),
                    remote_url: None,
                    remote_note: None,
                    branch: None,
                    head: None,
                    dirty: false,
                    unreadable: Some(clean(message)),
                });
            }
            _ => return Err(format!("unexpected output line {:?}", truncate(line))),
        }
    }
    if !done {
        return Err("the listing did not finish (no end marker)".into());
    }
    Ok(list)
}

fn truncate(line: &str) -> String {
    line.chars()
        .take(80)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

#[cfg(test)]
mod tests;
