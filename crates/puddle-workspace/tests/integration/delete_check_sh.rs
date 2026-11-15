// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `guest/delete-check.sh` under dash, bash and `busybox sh` (the Alpine guest's shell), against real git repositories in a
//! temp dir that plays the volume root, driven through [`Workspaces::check_delete`] on the fake
//! (its exec handler runs the script on the host). Needs `git` on the host.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};

use puddle_compute::fake::{ExecContext, FakeRuntime};
use puddle_compute::{DiskSize, ExecOutput, ExecRequest, Runtime, VolumeSpec};
use puddle_types::WorkspaceId;
use puddle_workspace::{DELETE_CHECK_SH, DeleteReport, Workspaces};

use crate::common;
use common::{command, git, volume};

/// The `shells()` entry that runs the script as `busybox sh`.
const BUSYBOX: &str = "busybox sh";

/// A `busybox` on the path, or none.
fn busybox_found() -> bool {
    command("busybox")
        .arg("true")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Dash and bash where present, plus `busybox sh` when a `busybox` binary exists.
fn shells() -> Vec<&'static str> {
    let mut shells = common::shells();
    if busybox_found() {
        shells.push(BUSYBOX);
    }
    shells
}

/// A directory whose `awk`, `tr` and `cut` are busybox applets, put first on the script's `PATH`:
/// the script then runs on the Alpine guest's userland, not the host's coreutils.
fn busybox_applets() -> &'static Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let which = command("sh")
            .args(["-c", "command -v busybox"])
            .output()
            .unwrap();
        let busybox = String::from_utf8(which.stdout).unwrap();
        for applet in ["awk", "tr", "cut"] {
            std::os::unix::fs::symlink(busybox.trim(), dir.path().join(applet)).unwrap();
        }
        dir
    })
    .path()
}

/// The shell command line: `busybox sh` is a program and an applet name, the others one program.
fn shell_command(shell: &str) -> std::process::Command {
    let Some(applet) = shell.strip_prefix("busybox ") else {
        return command(shell);
    };
    let mut c = command("busybox");
    c.arg(applet);
    let path = std::env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(busybox_applets().to_path_buf()).chain(std::env::split_paths(&path));
    c.env("PATH", std::env::join_paths(dirs).unwrap());
    c
}

/// Runs the check script with `shell` on `host_root` whenever the fake is asked to.
fn run_on_host(rt: &FakeRuntime, shell: &'static str, host_root: PathBuf) {
    rt.on_exec(move |_: &mut ExecContext<'_>, r: &ExecRequest| {
        if r.program != "sh" || r.args.get(1).map(String::as_str) != Some(DELETE_CHECK_SH) {
            return None;
        }
        assert_eq!(r.args.get(3).map(String::as_str), Some("/workspaces/acme"));
        // A blocking call in the fake's handler: fine for a test, the script takes milliseconds.
        let out = shell_command(shell)
            .args(["-c", DELETE_CHECK_SH, "puddle-delete-check"])
            .arg(&host_root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        Some(ExecOutput::new(
            out.status.code().unwrap_or(-1),
            out.stdout,
            out.stderr,
        ))
    });
}

async fn check(shell: &'static str, root: &Path) -> DeleteReport {
    let rt = FakeRuntime::new();
    run_on_host(&rt, shell, root.to_path_buf());
    let id = WorkspaceId::new("acme").unwrap();
    rt.create_volume(VolumeSpec {
        name: id.volume_name(),
        size: DiskSize::gib(1),
    })
    .await
    .unwrap();
    Workspaces::default().check_delete(&rt, &id).await.unwrap()
}

/// The Alpine guest has no dash or bash, only busybox's `sh`, so a missing `busybox` is no pass:
/// CI sets `PUDDLE_REQUIRE_BUSYBOX` and installs it; elsewhere the skip is printed with its reason.
#[test]
fn busybox_sh_is_there_to_run_the_script_under() {
    if busybox_found() {
        return;
    }
    assert!(
        std::env::var_os("PUDDLE_REQUIRE_BUSYBOX").is_none(),
        "PUDDLE_REQUIRE_BUSYBOX is set but no busybox is on the path"
    );
    eprintln!(
        "skipped: no busybox on the path; the delete-check tests ran under dash and bash only"
    );
}

#[tokio::test]
async fn the_script_finds_every_kind_of_unsaved_work() {
    let shells = shells();
    assert!(!shells.is_empty(), "no shell found");
    for shell in shells {
        let tmp = tempfile::tempdir().unwrap();
        let vol = volume(tmp.path());
        let report = check(shell, &vol).await;
        let f = &report.findings;
        let dirs: Vec<&str> = f.repos.iter().map(|r| r.dir.as_str()).collect();
        assert_eq!(
            dirs,
            ["api", "fresh", "web"],
            "{shell}: lost+found and .puddle skipped"
        );
        let api = &f.repos[0];
        let mut uncommitted = api.uncommitted.items.clone();
        uncommitted.sort();
        assert_eq!(uncommitted, [" M a.txt", "?? new.txt"], "{shell}");
        assert_eq!(api.unpushed.items.len(), 1, "{shell}");
        assert!(api.unpushed.items[0].ends_with(" local only"), "{shell}");
        assert_eq!(api.stashes.items.len(), 1, "{shell}");
        assert!(
            api.stashes.items[0].starts_with("stash@{0}: WIP on main"),
            "{shell}"
        );
        let fresh = &f.repos[1];
        assert_eq!(fresh.uncommitted.items, ["?? x"], "{shell}");
        assert!(
            fresh.unpushed.is_empty() && fresh.stashes.is_empty(),
            "{shell}"
        );
        assert!(f.repos[2].is_clean(), "{shell}: a fresh clone is clean");
        assert_eq!(f.other.items, ["notes.txt"], "{shell}");
        assert!(f.errors.is_empty(), "{shell}: {:?}", f.errors);
        assert!(!report.is_clean());
        assert!(
            !tmp.path().join("pwned").exists(),
            "{shell}: the repo's fsmonitor ran"
        );
    }
}

#[tokio::test]
async fn an_empty_volume_and_a_repo_without_remote_are_judged_right() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        std::fs::create_dir_all(vol.join("lost+found")).unwrap();
        assert!(check(shell, &vol).await.is_clean(), "{shell}");

        // Every commit of a repo without a remote is unsaved; so are commits on other branches.
        let solo = vol.join("solo");
        std::fs::create_dir(&solo).unwrap();
        git(&solo, &["init", "-q", "-b", "main"]);
        std::fs::write(solo.join("a"), "a\n").unwrap();
        git(&solo, &["add", "a"]);
        git(&solo, &["commit", "-qm", "one"]);
        git(&solo, &["checkout", "-q", "-b", "side"]);
        std::fs::write(solo.join("b"), "b\n").unwrap();
        git(&solo, &["add", "b"]);
        git(&solo, &["commit", "-qm", "two"]);
        git(&solo, &["checkout", "-q", "--detach", "main"]);
        let report = check(shell, &vol).await;
        let solo = &report.findings.repos[0];
        assert_eq!(solo.unpushed.total(), 2, "{shell}: {:?}", solo.unpushed);
        assert!(solo.uncommitted.is_empty(), "{shell}");
    }
}

#[tokio::test]
async fn a_commit_only_a_tag_holds_is_listed_and_a_git_ignored_file_is_not() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        let repo = vol.join("solo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(repo.join("a"), "a\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["checkout", "-q", "--detach"]);
        std::fs::write(repo.join("b"), "b\n").unwrap();
        git(&repo, &["add", "b"]);
        git(&repo, &["commit", "-qm", "only on a tag"]);
        git(&repo, &["tag", "keep-me"]);
        git(&repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("ignored.log"), "noise\n").unwrap();
        let report = check(shell, &vol).await;
        let solo = &report.findings.repos[0];
        assert!(
            solo.uncommitted.is_empty(),
            "{shell}: {:?}",
            solo.uncommitted
        );
        assert_eq!(solo.unpushed.total(), 2, "{shell}: {:?}", solo.unpushed);
        assert!(
            solo.unpushed
                .items
                .iter()
                .any(|c| c.ends_with(" only on a tag")),
            "{shell}: {:?}",
            solo.unpushed
        );
    }
}

#[tokio::test]
async fn a_shallow_clone_with_fetched_tags_lists_only_its_own_commit() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        for (file, tag) in [("one", "v1"), ("two", "v2"), ("three", "v3")] {
            std::fs::write(upstream.join(file), "x\n").unwrap();
            git(&upstream, &["add", "."]);
            git(&upstream, &["commit", "-qm", &format!("upstream {file}")]);
            git(&upstream, &["tag", tag]);
        }
        let vol = tmp.path().join("vol");
        std::fs::create_dir_all(&vol).unwrap();
        let url = format!("file://{}", upstream.display());
        git(&vol, &["clone", "-q", "--depth", "1", &url, "solo"]);
        let repo = vol.join("solo");
        git(&repo, &["fetch", "-q", "--tags"]);
        std::fs::write(repo.join("mine"), "m\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "local only"]);
        let report = check(shell, &vol).await;
        let solo = &report.findings.repos[0];
        let subjects: Vec<&str> = solo
            .unpushed
            .items
            .iter()
            .map(|c| c.split_once(' ').unwrap().1)
            .collect();
        assert_eq!(subjects, ["local only"], "{shell}: {:?}", solo.unpushed);
        assert_eq!(solo.unpushed.total(), 1, "{shell}");
    }
}

#[tokio::test]
async fn a_shallow_clone_lists_a_commit_only_a_local_tag_holds() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let upstream = tmp.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["init", "-q", "-b", "main"]);
        for (file, tag) in [("one", "v1"), ("two", "v2"), ("three", "v3")] {
            std::fs::write(upstream.join(file), "x\n").unwrap();
            git(&upstream, &["add", "."]);
            git(&upstream, &["commit", "-qm", &format!("upstream {file}")]);
            git(&upstream, &["tag", tag]);
        }
        let vol = tmp.path().join("vol");
        std::fs::create_dir_all(&vol).unwrap();
        let url = format!("file://{}", upstream.display());
        git(&vol, &["clone", "-q", "--depth", "1", &url, "solo"]);
        let repo = vol.join("solo");
        git(&repo, &["fetch", "-q", "--tags"]);
        std::fs::write(repo.join("mine"), "m\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "local only"]);
        git(&repo, &["checkout", "-q", "--detach"]);
        std::fs::write(repo.join("tagged"), "t\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "only on a tag"]);
        git(&repo, &["tag", "keep-me"]);
        git(&repo, &["checkout", "-q", "main"]);
        let report = check(shell, &vol).await;
        let solo = &report.findings.repos[0];
        let mut subjects: Vec<&str> = solo
            .unpushed
            .items
            .iter()
            .map(|c| c.split_once(' ').unwrap().1)
            .collect();
        subjects.sort_unstable();
        assert_eq!(
            subjects,
            ["local only", "only on a tag"],
            "{shell}: {:?}",
            solo.unpushed
        );
    }
}

#[tokio::test]
async fn long_lists_are_capped_and_counted() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        let repo = vol.join("big");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        for i in 0..250 {
            std::fs::write(repo.join(format!("f{i}")), "x").unwrap();
            std::fs::write(vol.join(format!("loose{i}")), "x").unwrap();
        }
        let report = check(shell, &vol).await;
        let big = &report.findings.repos[0];
        assert_eq!(big.uncommitted.items.len(), 200, "{shell}");
        assert_eq!(big.uncommitted.total(), 250, "{shell}");
        assert_eq!(report.findings.other.items.len(), 200, "{shell}");
        assert_eq!(report.findings.other.total(), 250, "{shell}");
    }
}

#[tokio::test]
async fn odd_top_level_names_cannot_break_the_output() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let vol = tmp.path().join("vol");
        std::fs::create_dir_all(&vol).unwrap();
        std::fs::write(vol.join("tab\there"), "x").unwrap();
        std::fs::write(vol.join("new\nline"), "x").unwrap();
        std::fs::write(vol.join(".hidden"), "x").unwrap();
        std::fs::write(vol.join("..dots"), "x").unwrap();
        let report = check(shell, &vol).await;
        let mut other = report.findings.other.items.clone();
        other.sort();
        assert_eq!(
            other,
            ["..dots", ".hidden", "new?line", "tab?here"],
            "{shell}"
        );
    }
}

#[tokio::test]
async fn a_missing_volume_root_fails_closed() {
    for shell in shells() {
        let tmp = tempfile::tempdir().unwrap();
        let rt = FakeRuntime::new();
        run_on_host(&rt, shell, tmp.path().join("missing"));
        let id = WorkspaceId::new("acme").unwrap();
        rt.create_volume(VolumeSpec {
            name: id.volume_name(),
            size: DiskSize::gib(1),
        })
        .await
        .unwrap();
        let err = Workspaces::default()
            .check_delete(&rt, &id)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot enter"), "{shell}: {err}");
    }
}
