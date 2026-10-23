// SPDX-License-Identifier: GPL-3.0-or-later
//! The real `guest/checkouts.sh` under dash and bash, against real git repositories in a temp
//! dir that plays the volume root, driven through [`Workspaces::list_checkouts`] on the fake (its
//! exec handler runs the script on the host). The fixture volume is the one the delete check's
//! tests use, so both scripts are held to the same idea of a checkout. A hostile guest is played
//! by an exec handler that answers with chosen text. Needs `git` on the host.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};

use puddle_compute::fake::{ExecContext, FakeRuntime, FakeSandbox};
use puddle_compute::{ExecOutput, ExecRequest, SandboxSpec};
use puddle_types::{CheckoutDir, ImageRef, SandboxName, WorkspaceId};
use puddle_workspace::{
    CHECKOUTS_SH, CheckoutList, DELETE_CHECK_SH, MAX_CHECKOUTS, MAX_FIELD, RemoteNote,
    WorkspaceError, Workspaces,
};

mod common;
use common::{command, git, shells, volume};

fn id() -> WorkspaceId {
    WorkspaceId::new("acme").unwrap()
}

/// A fake guest: the runtime, the workspace service that holds its volume, and the running
/// sandbox that has it.
struct Guest {
    rt: FakeRuntime,
    w: Workspaces,
    sb: FakeSandbox,
}

/// A running sandbox on a fake whose exec handler answers with `answer`; what it returns `None`
/// for is left to the fake.
async fn sandbox_answering(
    answer: impl Fn(&ExecRequest) -> Option<ExecOutput> + Send + Sync + 'static,
) -> Guest {
    let rt = FakeRuntime::new();
    rt.on_exec(move |_: &mut ExecContext<'_>, r: &ExecRequest| answer(r));
    let spec = SandboxSpec::new(
        SandboxName::new("box").unwrap(),
        ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
    );
    let w = Workspaces::default();
    let sb = w.create(&rt, &id(), spec, None).await.unwrap();
    Guest { rt, w, sb }
}

/// Runs `script` (a listing or the delete check) with `shell` on `host_root` instead of the guest's
/// mount point.
fn run_script(shell: &str, script: &str, r: &ExecRequest, host_root: &Path) -> ExecOutput {
    assert_eq!(r.args.get(3).map(String::as_str), Some("/workspaces/acme"));
    let out = command(shell)
        .args(["-c", script, "puddle-script"])
        .arg(host_root)
        .args(r.args.iter().skip(4))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    ExecOutput::new(out.status.code().unwrap_or(-1), out.stdout, out.stderr)
}

/// A sandbox whose guest runs the real listing and the real delete check with `shell`.
async fn sandbox_on(shell: &'static str, root: PathBuf) -> Guest {
    sandbox_answering(move |r| {
        if r.program != "sh" {
            return None;
        }
        match r.args.get(1).map(String::as_str) {
            Some(s) if s == CHECKOUTS_SH => Some(run_script(shell, CHECKOUTS_SH, r, &root)),
            Some(s) if s == DELETE_CHECK_SH => Some(run_script(shell, DELETE_CHECK_SH, r, &root)),
            _ => None,
        }
    })
    .await
}

async fn list(shell: &'static str, root: &Path) -> CheckoutList {
    let g = sandbox_on(shell, root.to_path_buf()).await;
    g.w.list_checkouts(&g.sb, &id()).await.unwrap()
}

#[tokio::test]
async fn the_listing_and_the_delete_check_agree_on_what_a_checkout_is() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = volume(tmp.path());
        // A link named like a checkout, and a plain folder: neither is one.
        std::os::unix::fs::symlink(vol.join("api"), vol.join("api-link")).unwrap();
        std::fs::create_dir(vol.join("plain-folder")).unwrap();
        let listed: Vec<String> = list(shell, &vol)
            .await
            .checkouts
            .into_iter()
            .map(|c| c.dir)
            .collect();
        let g = sandbox_on(shell, vol.clone()).await;
        let report = g.w.check_delete(&g.rt, &id()).await.unwrap();
        let deleted: Vec<&str> = report
            .findings
            .repos
            .iter()
            .map(|r| r.dir.as_str())
            .collect();
        assert_eq!(listed, deleted, "{shell}");
        assert_eq!(listed, ["api", "fresh", "web"], "{shell}");
    }
}

#[tokio::test]
async fn each_checkout_says_where_it_came_from_and_what_state_it_is_in() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = volume(tmp.path());
        let web = vol.join("web");
        git(
            &web,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/o/web.git",
            ],
        );
        let api = vol.join("api");
        git(
            &api,
            &["remote", "set-url", "origin", "https://u:tok@host/o/api"],
        );
        git(&api, &["remote", "add", "second", "https://host/o/other"]);
        let found = list(shell, &vol).await;
        assert_eq!(found.more, 0, "{shell}");
        let [api, fresh, web] = found.checkouts.as_slice() else {
            panic!("{shell}: {found:?}");
        };
        // origin wins over another remote; the credential never leaves the parser.
        assert_eq!(
            api.remote_url.as_deref(),
            Some("https://host/o/api"),
            "{shell}"
        );
        assert_eq!(
            api.remote_note,
            Some(RemoteNote::CredentialsRemoved),
            "{shell}"
        );
        assert_eq!(api.branch.as_deref(), Some("main"), "{shell}");
        assert!(api.head.is_some() && api.dirty, "{shell}: {api:?}");
        assert!(!format!("{found:?}").contains("tok"), "{shell}");
        // A repository with no remote and no commit.
        assert_eq!(fresh.remote_note, Some(RemoteNote::NoRemote), "{shell}");
        assert_eq!(fresh.head, None, "{shell}");
        assert!(fresh.dirty, "{shell}");
        // A clean clone.
        assert_eq!(
            web.remote_url.as_deref(),
            Some("https://github.com/o/web.git"),
            "{shell}"
        );
        assert_eq!(web.remote_note, None, "{shell}");
        assert!(!web.dirty, "{shell}");
        assert!(
            !tmp.path().join("pwned").exists(),
            "{shell}: the repo's fsmonitor ran"
        );
    }
}

#[tokio::test]
async fn the_only_remote_counts_without_an_origin_and_two_remotes_without_one_count_for_none() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        for (dir, remotes) in [
            ("only", vec![("up", "https://h/o/only")]),
            ("two", vec![("a", "https://h/o/a"), ("b", "https://h/o/b")]),
            ("ssh", vec![("origin", "git@github.com:o/r.git")]),
            ("local", vec![("origin", "/srv/git/r")]),
        ] {
            let repo = vol.join(dir);
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q"]);
            for (name, url) in remotes {
                git(&repo, &["remote", "add", name, url]);
            }
        }
        let found = list(shell, &vol).await;
        let by = |dir: &str| found.checkouts.iter().find(|c| c.dir == dir).unwrap();
        assert_eq!(
            by("only").remote_url.as_deref(),
            Some("https://h/o/only"),
            "{shell}"
        );
        assert_eq!(by("two").remote_note, Some(RemoteNote::NoRemote), "{shell}");
        assert_eq!(by("ssh").remote_note, Some(RemoteNote::Ssh), "{shell}");
        assert_eq!(by("ssh").remote_url, None, "{shell}");
        assert_eq!(
            by("local").remote_note,
            Some(RemoteNote::NotHttps),
            "{shell}"
        );
    }
}

#[tokio::test]
async fn one_directory_is_read_again_and_a_non_checkout_reads_as_nothing() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = volume(tmp.path());
        std::os::unix::fs::symlink(vol.join("api"), vol.join("linked")).unwrap();
        let g = sandbox_on(shell, vol.clone()).await;
        let (sb, w) = (&g.sb, &g.w);
        let dir = |name: &str| CheckoutDir::new(name).unwrap();
        let web = w
            .read_checkout(sb, &id(), &dir("web"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(web.dir, "web", "{shell}");
        for not in ["linked", "missing", "notes.txt", ".puddle", "lost+found"] {
            // Not every one of these is a valid checkout name; the script must still say nothing.
            let Ok(d) = CheckoutDir::new(not) else {
                continue;
            };
            assert_eq!(
                w.read_checkout(sb, &id(), &d).await.unwrap(),
                None,
                "{shell}: {not}"
            );
        }
    }
}

#[tokio::test]
async fn more_than_a_hundred_checkouts_list_a_hundred_and_count_the_rest() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        for i in 0..(MAX_CHECKOUTS + 50) {
            let repo = vol.join(format!("r{i:03}"));
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-q"]);
        }
        let found = list(shell, &vol).await;
        assert_eq!(found.checkouts.len(), MAX_CHECKOUTS, "{shell}");
        assert_eq!(found.more, 50, "{shell}");
    }
}

#[tokio::test]
async fn odd_names_and_remotes_cannot_break_the_output() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        let odd = vol.join("ta\tb\u{7}and\u{1b}esc");
        std::fs::create_dir_all(&odd).unwrap();
        git(&odd, &["init", "-q"]);
        git(
            &odd,
            &[
                "remote",
                "add",
                "origin",
                &format!("https://h/{}\u{7}\u{1b}[31mred", "x".repeat(2 * MAX_FIELD)),
            ],
        );
        let newline = vol.join("new\nline");
        std::fs::create_dir_all(&newline).unwrap();
        git(&newline, &["init", "-q"]);
        let found = list(shell, &vol).await;
        assert_eq!(found.checkouts.len(), 2, "{shell}: {found:?}");
        for c in &found.checkouts {
            for field in [Some(&c.dir), c.remote_url.as_ref(), c.branch.as_ref()]
                .into_iter()
                .flatten()
            {
                assert!(!field.chars().any(char::is_control), "{shell}: {field:?}");
                assert!(field.chars().count() <= MAX_FIELD, "{shell}");
            }
        }
        let mut dirs: Vec<&str> = found.checkouts.iter().map(|c| c.dir.as_str()).collect();
        dirs.sort_unstable();
        assert_eq!(dirs, ["new?line", "ta?b?and?esc"], "{shell}");
    }
}

#[tokio::test]
async fn a_missing_volume_root_fails_closed() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let g = sandbox_on(shell, tmp.path().join("missing")).await;
        let err = g.w.list_checkouts(&g.sb, &id()).await.unwrap_err();
        assert!(
            matches!(err, WorkspaceError::Checkouts { .. }),
            "{shell}: {err:?}"
        );
        assert!(err.to_string().contains("cannot enter"), "{shell}: {err}");
    }
}

/// What a hostile guest may answer with: none of it is shown in part.
#[tokio::test]
async fn a_hostile_guest_listing_is_refused_whole() {
    for (text, why) in [
        (
            "C\ta\t\t\t\t0\nD\n\nC\tb\t\t\t\t0\n",
            "after the end marker",
        ),
        ("C\ta\t\t\t\t0\n", "no end marker"),
        ("X\tforged\nD\n", "unexpected output line"),
        ("C\ta\t\t\t\t0\nZ\tb\nD\n", "unexpected output line"),
        ("C\ta\tb\tc\td\t0\textra\nD\n", "unexpected output line"),
        ("C\ta\t\t\t\tyes\nD\n", "unexpected output line"),
    ] {
        let canned = text.to_owned();
        let g = sandbox_answering(move |r| {
            (r.program == "sh" && r.args.get(1).map(String::as_str) == Some(CHECKOUTS_SH))
                .then(|| ExecOutput::new(0, canned.clone().into_bytes(), Vec::new()))
        })
        .await;
        let err = g.w.list_checkouts(&g.sb, &id()).await.unwrap_err();
        assert!(
            matches!(err, WorkspaceError::Checkouts { .. }),
            "{text:?}: {err:?}"
        );
        assert!(err.to_string().contains(why), "{text:?}: {err}");
    }
}

#[tokio::test]
async fn a_listing_that_fails_says_why_and_shows_nothing() {
    let g = sandbox_answering(|r| {
        (r.program == "sh" && r.args.get(1).map(String::as_str) == Some(CHECKOUTS_SH)).then(|| {
            ExecOutput::new(
                3,
                b"E\t.\tcannot enter /workspaces/acme\n".to_vec(),
                Vec::new(),
            )
        })
    })
    .await;
    let err = g.w.list_checkouts(&g.sb, &id()).await.unwrap_err();
    assert!(
        err.to_string().contains("cannot enter /workspaces/acme"),
        "{err}"
    );
}

#[tokio::test]
async fn something_on_the_volume_is_found_whatever_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let vol = tmp.path().join("vol");
    std::fs::create_dir_all(vol.join("folder")).unwrap();
    std::fs::write(vol.join("file"), "x").unwrap();
    std::os::unix::fs::symlink(vol.join("nowhere"), vol.join("dangling")).unwrap();
    let root = vol.clone();
    let g = sandbox_answering(move |r| {
        if r.program != "sh" || r.args.get(2).map(String::as_str) != Some("puddle-exists") {
            return None;
        }
        let name = r.args.get(3)?.strip_prefix("/workspaces/acme/")?;
        let out = command("sh")
            .args(["-c", r.args.get(1)?, "puddle-exists"])
            .arg(root.join(name))
            .output()
            .unwrap();
        Some(ExecOutput::new(
            out.status.code().unwrap_or(-1),
            Vec::new(),
            Vec::new(),
        ))
    })
    .await;
    let (sb, w) = (&g.sb, &g.w);
    for (name, there) in [
        ("folder", true),
        ("file", true),
        ("dangling", true),
        ("absent", false),
    ] {
        let dir = CheckoutDir::new(name).unwrap();
        assert_eq!(
            w.exists_on_volume(sb, &id(), &dir).await.unwrap(),
            there,
            "{name}"
        );
    }
}

#[tokio::test]
async fn not_knowing_whether_a_folder_is_there_is_an_error_not_a_no() {
    let g = sandbox_answering(|r| {
        (r.program == "sh" && r.args.get(2).map(String::as_str) == Some("puddle-exists"))
            .then(|| ExecOutput::new(2, Vec::new(), b"sh: input/output error\n".to_vec()))
    })
    .await;
    let dir = CheckoutDir::new("api").unwrap();
    let err = g.w.exists_on_volume(&g.sb, &id(), &dir).await.unwrap_err();
    assert!(
        err.to_string()
            .contains("looking for api exited 2: sh: input/output error"),
        "{err}"
    );
}

/// The fixtures must not touch the repository a git hook points `GIT_DIR` at.
#[test]
fn fixtures_leave_the_repository_of_a_git_hook_alone() {
    // skip-ok: the parent run starts this binary again as a child, and the child must not recurse.
    if common::is_hook_env_child() {
        return;
    }
    common::assert_hook_env_leaves_the_repository_alone(
        "fixtures_leave_the_repository_of_a_git_hook_alone",
    );
}
