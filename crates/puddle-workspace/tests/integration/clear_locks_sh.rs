// SPDX-License-Identifier: GPL-3.0-or-later
//! The real `guest/clear-locks.sh` under dash and bash, against real git repositories in a temp
//! dir that plays the volume root, driven through [`Workspaces::clear_stale_locks`] on the fake
//! (its exec handler runs the script on the host). A fake proc dir stands in for `/proc`. Needs
//! `git` on the host.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use puddle_compute::fake::{ExecContext, FakeRuntime};
use puddle_compute::{ExecOutput, ExecRequest, SandboxSpec};
use puddle_types::{ImageRef, SandboxName, WorkspaceId};
use puddle_workspace::{CLEAR_LOCKS_SH, LockReport, Workspaces};

use crate::common;
use common::command;

fn shells() -> Vec<&'static str> {
    ["dash", "bash"]
        .into_iter()
        .filter(|s| {
            Command::new("sh")
                .args(["-c", &format!("command -v {s}")])
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .collect()
}

/// Git for the fixture repositories, with background maintenance off: newer git starts a
/// detached `git maintenance run --auto` after a commit (and an auto gc can do the same), which
/// creates and deletes `.git/objects/maintenance.lock` after the commit has returned, so a test
/// that lists the locks could catch it between the two (seen with git 2.55).
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    command("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_CONFIG_KEY_1", "gc.auto")
        .env("GIT_CONFIG_VALUE_1", "0")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.org")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.org")
        .output()
        .unwrap()
}

fn git_ok(dir: &Path, args: &[&str]) {
    let out = git(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Fixture {
    tmp: tempfile::TempDir,
    vol: PathBuf,
    proc_dir: PathBuf,
}

/// A volume with: `api` (stale locks of several kinds, one nested), `web` (clean), `.puddle`
/// with a lock-named file, `lost+found`, a symlinked `.git`, a plain dir with a lock-named file,
/// and a hostile lock path with a newline. Plus an empty fake proc dir.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let vol = tmp.path().join("vol");
    let proc_dir = tmp.path().join("proc");
    std::fs::create_dir_all(&vol).unwrap();
    std::fs::create_dir_all(proc_dir.join("1")).unwrap();
    std::fs::write(proc_dir.join("1/comm"), "init\n").unwrap();
    std::fs::create_dir_all(proc_dir.join("2")).unwrap();
    std::fs::write(proc_dir.join("2/comm"), "bash\n").unwrap();
    std::fs::create_dir_all(proc_dir.join("net")).unwrap();
    for repo in ["api", "web"] {
        std::fs::create_dir(vol.join(repo)).unwrap();
        git_ok(&vol.join(repo), &["init", "-q", "-b", "main"]);
        std::fs::write(vol.join(repo).join("a.txt"), "a\n").unwrap();
        git_ok(&vol.join(repo), &["add", "."]);
        git_ok(&vol.join(repo), &["commit", "-qm", "first"]);
    }
    let api = vol.join("api/.git");
    for lock in [
        "index.lock",
        "HEAD.lock",
        "packed-refs.lock",
        "refs/heads/main.lock",
    ] {
        std::fs::write(api.join(lock), "").unwrap();
    }
    std::fs::create_dir_all(api.join("modules/sub/refs/heads")).unwrap();
    std::fs::write(api.join("modules/sub/index.lock"), "").unwrap();
    std::fs::write(api.join("refs/heads/odd\nname\t.lock"), "").unwrap();
    // Things that must survive.
    std::fs::write(api.join("keep.lockfile"), "x").unwrap();
    std::fs::write(vol.join("api/work.lock"), "user file").unwrap();
    std::fs::create_dir_all(vol.join(".puddle/code-server")).unwrap();
    std::fs::write(vol.join(".puddle/x.lock"), "").unwrap();
    std::fs::create_dir_all(vol.join("lost+found")).unwrap();
    std::fs::create_dir_all(vol.join("plain/.git")).unwrap();
    std::fs::write(vol.join("plain/.git/index.lock"), "").unwrap();
    // `plain` has a .git dir but isn't a repo: still a checkout dir, locks go. A symlinked .git is skipped.
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("index.lock"), "").unwrap();
    std::fs::create_dir(vol.join("linked")).unwrap();
    std::os::unix::fs::symlink(&outside, vol.join("linked/.git")).unwrap();
    std::os::unix::fs::symlink(&outside, vol.join("linkdir")).unwrap();
    Fixture { tmp, vol, proc_dir }
}

async fn run(shell: &'static str, fx: &Fixture) -> LockReport {
    let rt = FakeRuntime::new();
    let vol = fx.vol.clone();
    let proc_dir = fx.proc_dir.clone();
    rt.on_exec(move |_: &mut ExecContext<'_>, r: &ExecRequest| {
        if r.program != "sh" || r.args.get(1).map(String::as_str) != Some(CLEAR_LOCKS_SH) {
            return None;
        }
        assert_eq!(r.args.get(3).map(String::as_str), Some("/workspaces/acme"));
        let out = command(shell)
            .args(["-c", CLEAR_LOCKS_SH, "puddle-clear-locks"])
            .arg(&vol)
            .arg(&proc_dir)
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
    let id = WorkspaceId::new("acme").unwrap();
    let spec = SandboxSpec::new(
        SandboxName::new("box").unwrap(),
        ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
    );
    let w = Workspaces::default();
    let sb = w.create(&rt, &id, spec, None).await.unwrap();
    w.clear_stale_locks(&sb, &id).await.unwrap()
}

fn locks_under(root: &Path) -> Vec<String> {
    let out = command("find")
        .arg(root)
        .args(["-name", "*.lock", "-print0"])
        .output()
        .unwrap();
    let mut v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|l| !l.is_empty())
        .map(|l| l.strip_prefix(root.to_str().unwrap()).unwrap().to_owned())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn stale_locks_in_checkouts_are_removed_and_nothing_else() {
    for shell in shells() {
        let fx = fixture();
        let report = run(shell, &fx).await;
        assert!(!report.skipped_busy, "{shell}");
        assert!(report.errors.is_empty(), "{shell}: {:?}", report.errors);
        let mut removed = report.removed.clone();
        removed.sort();
        assert_eq!(
            removed,
            [
                "api/.git/HEAD.lock",
                "api/.git/index.lock",
                "api/.git/modules/sub/index.lock",
                "api/.git/packed-refs.lock",
                "api/.git/refs/heads/main.lock",
                "api/.git/refs/heads/odd?name?.lock",
                "plain/.git/index.lock",
            ],
            "{shell}"
        );
        // Left alone: user files, .puddle, a symlinked .git's target.
        assert_eq!(
            locks_under(&fx.vol),
            ["/.puddle/x.lock", "/api/work.lock"],
            "{shell}"
        );
        assert!(fx.vol.join("api/.git/keep.lockfile").exists(), "{shell}");
        assert!(
            fx.vol.parent().unwrap().join("outside/index.lock").exists(),
            "{shell}: followed a symlink out of the volume"
        );
    }
}

#[tokio::test]
async fn git_works_again_after_the_locks_are_cleared() {
    for shell in shells() {
        let fx = fixture();
        let api = fx.vol.join("api");
        assert!(
            !git(&api, &["commit", "--allow-empty", "-qm", "x"])
                .status
                .success()
        );
        run(shell, &fx).await;
        std::fs::write(api.join("b.txt"), "b\n").unwrap();
        git_ok(&api, &["add", "b.txt"]);
        git_ok(&api, &["commit", "-qm", "after the crash"]);
        git_ok(&api, &["fsck", "--full"]);
    }
}

#[tokio::test]
async fn nothing_is_removed_while_a_git_process_runs() {
    for shell in shells() {
        for comm in ["git", "git-remote-http", "git-credential-store"] {
            let fx = fixture();
            std::fs::create_dir_all(fx.proc_dir.join("77")).unwrap();
            std::fs::write(fx.proc_dir.join("77/comm"), format!("{comm}\n")).unwrap();
            let before = locks_under(&fx.vol);
            let report = run(shell, &fx).await;
            assert!(report.skipped_busy, "{shell} {comm}");
            assert_eq!(report.removed_count(), 0);
            assert_eq!(
                locks_under(&fx.vol),
                before,
                "{shell} {comm}: removed locks"
            );
        }
    }
}

#[tokio::test]
async fn processes_that_only_look_like_git_do_not_block() {
    for shell in shells() {
        let fx = fixture();
        for (pid, comm) in [("80", "gitk-helper"), ("81", "digit"), ("82", "my-git")] {
            std::fs::create_dir_all(fx.proc_dir.join(pid)).unwrap();
            std::fs::write(fx.proc_dir.join(pid).join("comm"), format!("{comm}\n")).unwrap();
        }
        assert!(!run(shell, &fx).await.skipped_busy, "{shell}");
    }
}

#[tokio::test]
async fn an_empty_volume_and_a_second_run_are_fine() {
    for shell in shells() {
        let fx = fixture();
        run(shell, &fx).await;
        let again = run(shell, &fx).await;
        assert_eq!(again.removed_count(), 0, "{shell}");
        assert_eq!(again.errors, Vec::<String>::new());
    }
}

/// A volume with one repository, `api`: two commits on `main`, a branch `feature` with a third,
/// `feature` checked out last (so `logs/HEAD` ends in a branch switch), a staged-then-committed
/// history and an unstaged edit plus an untracked file in the working tree.
fn repair_fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let vol = tmp.path().join("vol");
    let proc_dir = tmp.path().join("proc");
    std::fs::create_dir_all(&vol).unwrap();
    std::fs::create_dir_all(proc_dir.join("1")).unwrap();
    std::fs::write(proc_dir.join("1/comm"), "init\n").unwrap();
    let api = vol.join("api");
    std::fs::create_dir(&api).unwrap();
    git_ok(&api, &["init", "-q", "-b", "main"]);
    std::fs::write(api.join("a.txt"), "a\n").unwrap();
    git_ok(&api, &["add", "."]);
    git_ok(&api, &["commit", "-qm", "first"]);
    git_ok(&api, &["checkout", "-qb", "feature"]);
    std::fs::write(api.join("b.txt"), "b\n").unwrap();
    git_ok(&api, &["add", "."]);
    git_ok(&api, &["commit", "-qm", "second"]);
    git_ok(&api, &["checkout", "-q", "main"]);
    git_ok(&api, &["checkout", "-q", "feature"]);
    std::fs::write(api.join("a.txt"), "edited, not staged\n").unwrap();
    std::fs::write(api.join("new.txt"), "untracked\n").unwrap();
    Fixture { tmp, vol, proc_dir }
}

/// Overwrites `path` with `len` NUL bytes, as a power cut leaves a renamed file.
fn zero_fill(path: &Path, len: usize) {
    std::fs::write(path, vec![0_u8; len]).unwrap();
}

fn git_text(dir: &Path, args: &[&str]) -> String {
    let out = git(dir, args);
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_owned()
}

#[tokio::test]
async fn a_zero_filled_head_is_rewritten_from_the_reflog() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let tip = git_text(&api, &["rev-parse", "HEAD"]);
        zero_fill(&api.join(".git/HEAD"), 24);
        let red = git(&api, &["status"]);
        assert!(!red.status.success(), "{shell}: git still works");
        assert!(
            String::from_utf8_lossy(&red.stderr).contains("not a git repository"),
            "{shell}: {}",
            String::from_utf8_lossy(&red.stderr)
        );
        let report = run(shell, &fx).await;
        assert_eq!(
            report.repaired,
            ["api/.git/HEAD: HEAD was zero-filled, rewritten: branch feature"],
            "{shell}"
        );
        assert!(
            report.damaged.is_empty() && report.errors.is_empty(),
            "{shell}"
        );
        assert_eq!(
            std::fs::read_to_string(api.join(".git/HEAD")).unwrap(),
            "ref: refs/heads/feature\n"
        );
        assert_eq!(git_text(&api, &["rev-parse", "HEAD"]), tip, "{shell}");
        git_ok(&api, &["fsck", "--full"]);
        // The working tree is as the user left it.
        assert_eq!(
            std::fs::read_to_string(api.join("a.txt")).unwrap(),
            "edited, not staged\n"
        );
        assert!(api.join("new.txt").exists(), "{shell}");
    }
}

#[tokio::test]
async fn an_empty_head_is_rewritten_too() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        std::fs::write(api.join(".git/HEAD"), "").unwrap();
        let report = run(shell, &fx).await;
        assert_eq!(report.repaired.len(), 1, "{shell}: {report:?}");
        git_ok(&api, &["fsck", "--full"]);
        assert_eq!(git_text(&api, &["branch", "--show-current"]), "feature");
    }
}

#[tokio::test]
async fn a_zero_filled_head_of_a_detached_checkout_is_rewritten_detached() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        git_ok(&api, &["commit", "--allow-empty", "-qm", "third"]);
        let tip = git_text(&api, &["rev-parse", "feature~1"]);
        git_ok(&api, &["checkout", "-q", &tip]);
        zero_fill(&api.join(".git/HEAD"), 41);
        let report = run(shell, &fx).await;
        assert_eq!(
            report.repaired,
            [format!(
                "api/.git/HEAD: HEAD was zero-filled, rewritten: detached at {tip}"
            )],
            "{shell}"
        );
        assert_eq!(git_text(&api, &["rev-parse", "HEAD"]), tip, "{shell}");
        assert_eq!(git_text(&api, &["branch", "--show-current"]), "");
        git_ok(&api, &["fsck", "--full"]);
    }
}

#[tokio::test]
async fn a_zero_filled_index_is_rebuilt_without_touching_the_working_tree() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        zero_fill(&api.join(".git/index"), 1234);
        let red = git(&api, &["status"]);
        assert!(!red.status.success(), "{shell}: git still works");
        let report = run(shell, &fx).await;
        assert_eq!(
            report.repaired,
            ["api/.git/index: index was damaged, rebuilt from HEAD (staged changes are not kept)"],
            "{shell}"
        );
        assert_eq!(
            git_text(&api, &["status", "--porcelain"]),
            " M a.txt\n?? new.txt",
            "{shell}"
        );
        assert_eq!(
            std::fs::read_to_string(api.join("a.txt")).unwrap(),
            "edited, not staged\n"
        );
        git_ok(&api, &["fsck", "--full"]);
        // Nothing was in the zero-filled file worth keeping.
        assert!(
            std::fs::read_dir(api.join(".git")).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("index.puddle-corrupt")),
            "{shell}"
        );
    }
}

#[tokio::test]
async fn a_corrupt_index_is_rebuilt_and_the_damaged_file_is_kept() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let index = api.join(".git/index");
        let mut bytes = std::fs::read(&index).unwrap();
        // The second half is lost, as when a power cut keeps a file's size and not its blocks.
        let mid = bytes.len() / 2;
        bytes[mid..].fill(0);
        std::fs::write(&index, &bytes).unwrap();
        assert!(!git(&api, &["status"]).status.success(), "{shell}");
        let report = run(shell, &fx).await;
        assert_eq!(report.repaired.len(), 1, "{shell}: {report:?}");
        git_ok(&api, &["status"]);
        git_ok(&api, &["fsck", "--full"]);
        let kept: Vec<_> = std::fs::read_dir(api.join(".git"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("index.puddle-corrupt"))
            .collect();
        assert_eq!(kept.len(), 1, "{shell}: {kept:?}");
        assert_eq!(
            std::fs::read(api.join(".git").join(&kept[0])).unwrap(),
            bytes,
            "{shell}"
        );
    }
}

#[tokio::test]
async fn an_index_with_an_extension_a_newer_git_wrote_is_left_alone() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let index = api.join(".git/index");
        let mut bytes = std::fs::read(&index).unwrap();
        bytes.truncate(bytes.len() - 20);
        bytes.extend_from_slice(b"abcd");
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&[0; 20]);
        std::fs::write(&index, &bytes).unwrap();
        assert!(
            !git(&api, &["status"]).status.success(),
            "{shell}: git reads it"
        );
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert!(
            report.damaged[0].contains("extension"),
            "{shell}: {:?}",
            report.damaged
        );
        assert_eq!(std::fs::read(&index).unwrap(), bytes, "{shell}");
    }
}

#[tokio::test]
async fn head_and_index_are_both_repaired_in_one_run_and_a_second_run_does_nothing() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        zero_fill(&api.join(".git/HEAD"), 24);
        zero_fill(&api.join(".git/index"), 500);
        let report = run(shell, &fx).await;
        assert_eq!(report.repaired.len(), 2, "{shell}: {report:?}");
        git_ok(&api, &["fsck", "--full"]);
        git_ok(&api, &["commit", "--allow-empty", "-qm", "after the cut"]);
        let again = run(shell, &fx).await;
        assert!(
            again.repaired.is_empty() && again.damaged.is_empty(),
            "{shell}"
        );
    }
}

#[tokio::test]
async fn a_healthy_repository_is_not_touched() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let head = std::fs::read(api.join(".git/HEAD")).unwrap();
        let index = std::fs::read(api.join(".git/index")).unwrap();
        let report = run(shell, &fx).await;
        assert!(
            report.repaired.is_empty() && report.damaged.is_empty(),
            "{shell}"
        );
        assert_eq!(std::fs::read(api.join(".git/HEAD")).unwrap(), head);
        assert_eq!(std::fs::read(api.join(".git/index")).unwrap(), index);
    }
}

#[tokio::test]
async fn a_head_whose_last_logged_commit_is_missing_is_left_alone_and_reported() {
    for shell in shells() {
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let log = api.join(".git/logs/HEAD");
        let text = std::fs::read_to_string(&log).unwrap();
        let line = text.lines().last().unwrap();
        let tip = line.split(' ').nth(1).unwrap();
        let hostile = text.replace(tip, &"1".repeat(40));
        std::fs::write(&log, hostile).unwrap();
        zero_fill(&api.join(".git/HEAD"), 24);
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert_eq!(report.damaged.len(), 1, "{shell}: {report:?}");
        assert!(
            report.damaged[0].starts_with("api/.git/HEAD: HEAD is damaged and the last commit"),
            "{shell}: {:?}",
            report.damaged
        );
        assert_eq!(
            std::fs::read(api.join(".git/HEAD")).unwrap(),
            vec![0_u8; 24]
        );
    }
}

#[tokio::test]
async fn a_head_is_left_alone_when_the_branch_moved_or_the_log_is_unreadable() {
    for shell in shells() {
        // The branch the log says was checked out points somewhere else now.
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        let main = git_text(&api, &["rev-parse", "main"]);
        std::fs::write(api.join(".git/refs/heads/feature"), format!("{main}\n")).unwrap();
        zero_fill(&api.join(".git/HEAD"), 24);
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert!(report.damaged[0].contains("no longer points"), "{shell}");
        // A log with garbage, and a missing log.
        for log in [Some(&b"\0\0\0\0"[..]), None] {
            let fx = repair_fixture();
            let api = fx.vol.join("api");
            match log {
                Some(bytes) => std::fs::write(api.join(".git/logs/HEAD"), bytes).unwrap(),
                None => std::fs::remove_file(api.join(".git/logs/HEAD")).unwrap(),
            }
            zero_fill(&api.join(".git/HEAD"), 24);
            let report = run(shell, &fx).await;
            assert!(report.repaired.is_empty(), "{shell}: {report:?}");
            assert_eq!(report.damaged.len(), 1, "{shell}: {report:?}");
            assert_eq!(
                std::fs::read(api.join(".git/HEAD")).unwrap(),
                vec![0_u8; 24]
            );
        }
    }
}

#[tokio::test]
async fn a_head_is_left_alone_when_its_branch_is_unreadable_or_may_have_been_renamed() {
    for shell in shells() {
        // The branch file is zero-filled as well: the commit alone cannot say it was a branch.
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        zero_fill(&api.join(".git/refs/heads/feature"), 41);
        zero_fill(&api.join(".git/HEAD"), 24);
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert!(
            report.damaged[0].contains("unreadable"),
            "{shell}: {:?}",
            report.damaged
        );
        // The checked-out branch was renamed after the last checkout entry.
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        git_ok(&api, &["branch", "-m", "renamed"]);
        zero_fill(&api.join(".git/HEAD"), 24);
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert!(
            report.damaged[0].contains("renamed branch"),
            "{shell}: {:?}",
            report.damaged
        );
        // A commit message that looks like a checkout entry is not one.
        let fx = repair_fixture();
        let api = fx.vol.join("api");
        git_ok(
            &api,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                "checkout: moving from a to b",
            ],
        );
        zero_fill(&api.join(".git/HEAD"), 24);
        let report = run(shell, &fx).await;
        assert_eq!(report.repaired.len(), 1, "{shell}: {report:?}");
        assert_eq!(git_text(&api, &["branch", "--show-current"]), "feature");
    }
}

#[tokio::test]
async fn an_index_is_left_alone_when_a_rebuild_would_lose_state() {
    for shell in shells() {
        for state in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "rebase-merge"] {
            let fx = repair_fixture();
            let api = fx.vol.join("api");
            let path = api.join(".git").join(state);
            if state == "rebase-merge" {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(&path, "0000000000000000000000000000000000000001\n").unwrap();
            }
            zero_fill(&api.join(".git/index"), 300);
            let report = run(shell, &fx).await;
            assert!(report.repaired.is_empty(), "{shell} {state}: {report:?}");
            assert_eq!(report.damaged.len(), 1, "{shell} {state}");
            assert!(
                report.damaged[0].contains(state),
                "{shell}: {:?}",
                report.damaged
            );
            assert_eq!(
                std::fs::read(api.join(".git/index")).unwrap(),
                vec![0_u8; 300]
            );
        }
        // A repository with no commit: nothing to rebuild from.
        let fx = repair_fixture();
        let fresh = fx.vol.join("fresh");
        std::fs::create_dir(&fresh).unwrap();
        git_ok(&fresh, &["init", "-q", "-b", "main"]);
        zero_fill(&fresh.join(".git/index"), 300);
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert!(
            report.damaged[0].contains("no commit"),
            "{shell}: {:?}",
            report.damaged
        );
    }
}

#[tokio::test]
async fn a_git_folder_that_is_not_a_repository_is_not_reported() {
    for shell in shells() {
        let fx = repair_fixture();
        let plain = fx.vol.join("plain/.git");
        std::fs::create_dir_all(&plain).unwrap();
        zero_fill(&plain.join("HEAD"), 10);
        zero_fill(&plain.join("index"), 10);
        let report = run(shell, &fx).await;
        assert!(
            report.repaired.is_empty() && report.damaged.is_empty(),
            "{shell}: {report:?}"
        );
        assert_eq!(std::fs::read(plain.join("HEAD")).unwrap(), vec![0_u8; 10]);
    }
}

#[tokio::test]
async fn every_checkout_of_the_volume_is_repaired() {
    for shell in shells() {
        let fx = repair_fixture();
        let web = fx.vol.join("web");
        std::fs::create_dir(&web).unwrap();
        git_ok(&web, &["init", "-q", "-b", "trunk"]);
        std::fs::write(web.join("w.txt"), "w\n").unwrap();
        git_ok(&web, &["add", "."]);
        git_ok(&web, &["commit", "-qm", "w"]);
        git_ok(&web, &["checkout", "-qb", "side"]);
        for repo in ["api", "web"] {
            zero_fill(&fx.vol.join(repo).join(".git/index"), 100);
        }
        zero_fill(&web.join(".git/HEAD"), 20);
        let report = run(shell, &fx).await;
        let mut repaired = report.repaired.clone();
        repaired.sort();
        assert_eq!(repaired.len(), 3, "{shell}: {report:?}");
        for repo in ["api", "web"] {
            git_ok(&fx.vol.join(repo), &["status"]);
            git_ok(&fx.vol.join(repo), &["fsck", "--full"]);
        }
        assert_eq!(git_text(&web, &["branch", "--show-current"]), "side");
    }
}

/// `repair_fixture` plus a linked worktree `wt` of `api` (branch `wtb`, switched to and from
/// `main` so `logs/HEAD` ends in a branch switch) and a submodule `api/sub` (its git dir is
/// `api/.git/modules/sub`, which `api/sub/.git` points at).
fn linked_fixture() -> Fixture {
    let fx = repair_fixture();
    let api = fx.vol.join("api");
    git_ok(&api, &["worktree", "add", "-q", "../wt", "-b", "wtb"]);
    let wt = fx.vol.join("wt");
    std::fs::write(wt.join("w.txt"), "w\n").unwrap();
    git_ok(&wt, &["add", "w.txt"]);
    git_ok(&wt, &["commit", "-qm", "in the worktree"]);
    git_ok(&wt, &["checkout", "-q", "--detach"]);
    git_ok(&wt, &["checkout", "-q", "wtb"]);
    std::fs::write(wt.join("w.txt"), "edited, not staged\n").unwrap();

    let lib = fx.tmp.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    git_ok(&lib, &["init", "-q", "-b", "main"]);
    std::fs::write(lib.join("l.txt"), "l\n").unwrap();
    git_ok(&lib, &["add", "."]);
    git_ok(&lib, &["commit", "-qm", "lib"]);
    git_ok(
        &api,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "sub",
        ],
    );
    // A branch switch in the submodule, for the same reason.
    git_ok(&api.join("sub"), &["checkout", "-qb", "side"]);
    git_ok(&api.join("sub"), &["checkout", "-q", "main"]);
    git_ok(&api.join("sub"), &["checkout", "-q", "side"]);
    fx
}

#[tokio::test]
async fn a_linked_worktree_and_a_submodule_git_dir_are_repaired() {
    for shell in shells() {
        let fx = linked_fixture();
        let (api, wt, sub) = (
            fx.vol.join("api"),
            fx.vol.join("wt"),
            fx.vol.join("api/sub"),
        );
        let wt_gd = api.join(".git/worktrees/wt");
        let sub_gd = api.join(".git/modules/sub");
        let wt_tip = git_text(&wt, &["rev-parse", "HEAD"]);
        let sub_tip = git_text(&sub, &["rev-parse", "HEAD"]);
        let api_head = std::fs::read(api.join(".git/HEAD")).unwrap();
        let api_index = std::fs::read(api.join(".git/index")).unwrap();
        for gd in [&wt_gd, &sub_gd] {
            zero_fill(&gd.join("HEAD"), 24);
            zero_fill(&gd.join("index"), 600);
        }
        assert!(!git(&wt, &["status"]).status.success(), "{shell}");
        assert!(!git(&sub, &["status"]).status.success(), "{shell}");

        let report = run(shell, &fx).await;
        let mut repaired = report.repaired.clone();
        repaired.sort();
        assert_eq!(
            repaired,
            [
                "api/.git/modules/sub/HEAD: HEAD was zero-filled, rewritten: branch side",
                "api/.git/modules/sub/index: index was damaged, rebuilt from HEAD (staged changes are not kept)",
                "api/.git/worktrees/wt/HEAD: HEAD was zero-filled, rewritten: branch wtb",
                "api/.git/worktrees/wt/index: index was damaged, rebuilt from HEAD (staged changes are not kept)",
            ],
            "{shell}: {report:?}"
        );
        assert!(
            report.damaged.is_empty() && report.errors.is_empty(),
            "{shell}: {report:?}"
        );
        assert_eq!(git_text(&wt, &["rev-parse", "HEAD"]), wt_tip, "{shell}");
        assert_eq!(git_text(&wt, &["branch", "--show-current"]), "wtb");
        assert_eq!(git_text(&sub, &["rev-parse", "HEAD"]), sub_tip, "{shell}");
        assert_eq!(git_text(&sub, &["branch", "--show-current"]), "side");
        git_ok(&wt, &["fsck", "--full"]);
        git_ok(&sub, &["fsck", "--full"]);
        // The working trees are as the user left them.
        assert_eq!(
            std::fs::read_to_string(wt.join("w.txt")).unwrap(),
            "edited, not staged\n"
        );
        assert!(git_text(&wt, &["status", "--porcelain"]).contains(" M w.txt"));
        // The main repository was healthy and is byte for byte what it was.
        assert_eq!(std::fs::read(api.join(".git/HEAD")).unwrap(), api_head);
        assert_eq!(std::fs::read(api.join(".git/index")).unwrap(), api_index);
        let again = run(shell, &fx).await;
        assert!(
            again.repaired.is_empty() && again.damaged.is_empty(),
            "{shell}: {again:?}"
        );
    }
}

#[tokio::test]
async fn healthy_linked_worktrees_and_submodules_are_not_touched() {
    for shell in shells() {
        let fx = linked_fixture();
        let api = fx.vol.join("api");
        let files = [
            ".git/worktrees/wt/HEAD",
            ".git/worktrees/wt/index",
            ".git/modules/sub/HEAD",
            ".git/modules/sub/index",
        ];
        let before: Vec<_> = files
            .iter()
            .map(|f| std::fs::read(api.join(f)).unwrap())
            .collect();
        let report = run(shell, &fx).await;
        assert!(
            report.repaired.is_empty() && report.damaged.is_empty() && report.errors.is_empty(),
            "{shell}: {report:?}"
        );
        for (f, b) in files.iter().zip(before) {
            assert_eq!(std::fs::read(api.join(f)).unwrap(), b, "{shell}: {f}");
        }
    }
}

#[tokio::test]
async fn a_gitdir_pointer_that_leaves_the_volume_is_reported_and_not_followed() {
    for shell in shells() {
        let fx = linked_fixture();
        let outside = fx.tmp.path().join("outside-git");
        std::fs::create_dir_all(outside.join("objects")).unwrap();
        std::fs::create_dir_all(outside.join("refs")).unwrap();
        zero_fill(&outside.join("HEAD"), 12);
        std::fs::create_dir(fx.vol.join("evil")).unwrap();
        std::fs::write(
            fx.vol.join("evil/.git"),
            format!("gitdir: {}\n", outside.display()),
        )
        .unwrap();
        // A pointer through a symlink inside the volume is refused as well.
        std::fs::create_dir(fx.vol.join("sneaky")).unwrap();
        std::os::unix::fs::symlink(&outside, fx.vol.join("sneaky/gd")).unwrap();
        std::fs::write(fx.vol.join("sneaky/.git"), "gitdir: gd\n").unwrap();
        let report = run(shell, &fx).await;
        assert!(report.repaired.is_empty(), "{shell}: {report:?}");
        assert_eq!(report.damaged.len(), 2, "{shell}: {report:?}");
        assert_eq!(
            std::fs::read(outside.join("HEAD")).unwrap(),
            vec![0_u8; 12],
            "{shell}"
        );
    }
}
