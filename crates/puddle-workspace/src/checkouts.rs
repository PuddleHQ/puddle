// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn a_full_listing_parses() {
        let out = "C\tapi\thttps://github.com/o/api.git\tmain\tabc1234\t0\n\
                   C\tweb\thttps://u:tok@host/o/web\t\tdef5678\t1\n\
                   C\tfresh\t\tmaster\t\t0\n\
                   C\tssh\tgit@github.com:o/r.git\tmain\tabc\t0\n\
                   C\tlocal\t/srv/repo\tmain\tabc\t0\n\
                   C\tplain\thttp://host/r\tmain\tabc\t0\n\
                   E\tbroken\tgit remote failed: boom\n\
                   M\t7\nD\n";
        let list = parse(out).unwrap();
        assert_eq!(list.more, 7);
        let by = |dir: &str| list.checkouts.iter().find(|c| c.dir == dir).unwrap();
        let api = by("api");
        assert_eq!(
            api.remote_url.as_deref(),
            Some("https://github.com/o/api.git")
        );
        assert_eq!(api.remote_note, None);
        assert_eq!(api.branch.as_deref(), Some("main"));
        assert_eq!(api.head.as_deref(), Some("abc1234"));
        assert!(!api.dirty);
        let web = by("web");
        assert_eq!(web.remote_url.as_deref(), Some("https://host/o/web"));
        assert_eq!(web.remote_note, Some(RemoteNote::CredentialsRemoved));
        assert_eq!(web.branch, None, "detached");
        assert!(web.dirty);
        let fresh = by("fresh");
        assert_eq!(
            (fresh.remote_url.as_deref(), fresh.head.as_deref()),
            (None, None)
        );
        assert_eq!(fresh.remote_note, Some(RemoteNote::NoRemote));
        assert_eq!(by("ssh").remote_note, Some(RemoteNote::Ssh));
        assert_eq!(by("local").remote_note, Some(RemoteNote::NotHttps));
        assert_eq!(by("plain").remote_note, Some(RemoteNote::NotHttps));
        assert_eq!(by("plain").remote_url, None);
        assert_eq!(
            by("broken").unreadable.as_deref(),
            Some("git remote failed: boom")
        );
    }

    #[test]
    fn an_empty_volume_lists_nothing() {
        assert_eq!(parse("D\n").unwrap(), CheckoutList::default());
    }

    #[test]
    fn credentials_never_survive_the_parser() {
        for (raw, shown) in [
            ("https://user:tok@host/x", "https://host/x"),
            ("https://tok@host/x", "https://host/x"),
            ("https://a:b@c@host/x?y=1", "https://host/x?y=1"),
            ("HTTPS://u:p@Host:8443/x", "HTTPS://Host:8443/x"),
        ] {
            let out = format!("C\tr\t{raw}\tmain\tabc\t0\nD\n");
            let found = &parse(&out).unwrap().checkouts[0];
            assert_eq!(found.remote_url.as_deref(), Some(shown), "{raw}");
            assert_eq!(
                found.remote_note,
                Some(RemoteNote::CredentialsRemoved),
                "{raw}"
            );
            assert!(!format!("{found:?}").contains("tok"), "{raw}");
        }
        // An at sign after the host is part of the path, not credentials.
        let out = "C\tr\thttps://host/x@y\tmain\tabc\t0\nD\n";
        let found = &parse(out).unwrap().checkouts[0];
        assert_eq!(found.remote_url.as_deref(), Some("https://host/x@y"));
        assert_eq!(found.remote_note, None);
        // An SSH address with a user name shows nothing at all.
        let out = "C\tr\tssh://git:pw@host/x\tmain\tabc\t0\nD\n";
        let found = &parse(out).unwrap().checkouts[0];
        assert_eq!(
            (found.remote_url.as_deref(), found.remote_note),
            (None, Some(RemoteNote::Ssh))
        );
    }

    #[test]
    fn malformed_or_unfinished_output_is_refused_whole() {
        for (out, want) in [
            ("", "no end marker"),
            ("C\ta\t\t\t\t0\n", "no end marker"),
            ("X\ty\nD\n", "unexpected output line"),
            ("C\ta\t\t\t\t2\nD\n", "unexpected output line"),
            ("C\ta\t\t\t0\nD\n", "unexpected output line"),
            ("C\ta\tb\tc\td\t0\textra\nD\n", "unexpected output line"),
            ("C\t\t\t\t\t0\nD\n", "unexpected output line"),
            ("M\tmany\nD\n", "bad count line"),
            ("M\nD\n", "unexpected output line"),
            ("E\ta\nD\n", "unexpected output line"),
            ("D\nC\ta\t\t\t\t0\n", "after the end marker"),
            ("D\tx\n", "unexpected output line"),
            // A forged record kind that only differs in case.
            ("c\ta\t\t\t\t0\nD\n", "unexpected output line"),
        ] {
            let err = parse(out).unwrap_err();
            assert!(err.contains(want), "{out:?}: {err}");
        }
    }

    #[test]
    fn hostile_output_stays_bounded_and_printable() {
        let long = "x".repeat(5000);
        let out = format!("C\td\u{7}ir\thttps://h/{long}\tbra\u{1b}nch\t{long}\t0\nD\n");
        let found = &parse(&out).unwrap().checkouts[0];
        assert_eq!(found.dir, "d?ir");
        assert_eq!(found.branch.as_deref(), Some("bra?nch"));
        for field in [
            Some(&found.dir),
            found.remote_url.as_ref(),
            found.branch.as_ref(),
            found.head.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(field.chars().count() <= MAX_FIELD, "{field}");
            assert!(!field.chars().any(char::is_control));
        }

        let mut many = String::new();
        for i in 0..=MAX_CHECKOUTS {
            writeln!(many, "C\tr{i}\t\t\t\t0").unwrap();
        }
        assert!(
            parse(&format!("{many}D\n"))
                .unwrap_err()
                .contains("checkouts")
        );

        let mut errors = String::new();
        for i in 0..300 {
            writeln!(errors, "E\tr{i}\t{long}").unwrap();
        }
        let list = parse(&format!("{errors}D\n")).unwrap();
        assert_eq!(list.checkouts.len(), MAX_ERRORS);
        assert!(list.checkouts.iter().all(|c| {
            c.unreadable
                .as_ref()
                .is_some_and(|m| m.chars().count() <= MAX_FIELD)
        }));
        let out = "M\t18446744073709551615\nM\t5\nD\n";
        assert_eq!(parse(out).unwrap().more, u64::MAX, "saturates");
    }

    proptest::proptest! {
        /// Whatever the guest prints, the answer is a listing or one error, and no field it
        /// keeps is longer than the limit or holds a control character.
        #[test]
        fn any_output_is_a_bounded_listing_or_one_error(text in "(?s).{0,3000}") {
            check_bounded(&text);
        }

        /// The same, for text that is shaped like records, so the parser's branches run.
        #[test]
        fn record_shaped_output_is_a_bounded_listing_or_one_error(
            lines in proptest::collection::vec(
                (
                    proptest::sample::select(vec!["C", "M", "E", "D", "X"]),
                    proptest::collection::vec("(?s).{0,700}", 0..7),
                ),
                0..12,
            ),
        ) {
            let mut text = String::new();
            for (kind, fields) in &lines {
                writeln!(text, "{kind}\t{}", fields.join("\t")).unwrap();
            }
            check_bounded(&text);
        }
    }

    fn check_bounded(text: &str) {
        let Ok(list) = parse(text) else { return };
        assert!(list.checkouts.len() <= MAX_CHECKOUTS.max(MAX_ERRORS) * 2);
        for c in &list.checkouts {
            for field in [
                Some(&c.dir),
                c.remote_url.as_ref(),
                c.branch.as_ref(),
                c.head.as_ref(),
                c.unreadable.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                assert!(field.chars().count() <= MAX_FIELD);
                assert!(!field.chars().any(char::is_control));
            }
        }
    }

    #[test]
    fn the_request_runs_the_script_as_root_on_the_mount_point() {
        let l = Layout::new(&WorkspaceId::new("a").unwrap()).unwrap();
        let r = request(&l, None);
        assert_eq!(r.program, "sh");
        assert_eq!(
            r.args,
            ["-c", CHECKOUTS_SH, "puddle-checkouts", "/workspaces/a"]
        );
        assert_eq!(r.user.as_deref(), Some("root"));
        let one = request(&l, Some(&CheckoutDir::new("src").unwrap()));
        assert_eq!(one.args.last().map(String::as_str), Some("src"));
        assert_eq!(one.args.len(), 5);
    }
}
