// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared by the tests that run `git` (or scripts that do) on the host.
//!
//! Under a git hook (the pre-push gates run the tests) git exports `GIT_DIR`, `GIT_INDEX_FILE`
//! and friends. Every `git` the tests spawn would then act on Puddle's own repository whatever
//! its working directory is: `git init --bare` in a temp dir rewrites that repository's
//! `core.bare`. [`command`] and [`git_command`] drop every inherited `GIT_*` variable.
#![allow(dead_code, reason = "each test binary uses a part of this module")]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Set in the child run of [`assert_hook_env_leaves_the_repository_alone`].
const CHILD_MARKER: &str = "PUDDLE_TEST_HOOK_ENV_CHILD";

/// `program` with every `GIT_*` variable in `inherited` removed.
pub(crate) fn command_without(
    program: &str,
    inherited: impl IntoIterator<Item = OsString>,
) -> Command {
    let mut cmd = Command::new(program);
    for key in inherited {
        if key.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(key);
        }
    }
    cmd
}

/// `program` without the caller's `GIT_*` variables.
pub(crate) fn command(program: &str) -> Command {
    command_without(program, std::env::vars_os().map(|(key, _)| key))
}

/// `git` without the caller's `GIT_*` variables.
pub(crate) fn git_command() -> Command {
    command("git")
}

/// True in the child run, where the regression test itself must not recurse.
pub(crate) fn is_hook_env_child() -> bool {
    std::env::var_os(CHILD_MARKER).is_some()
}

/// Regression for the leak: runs this very test binary again (every test but `skip`) with the
/// variables a git hook exports, pointing at a throwaway repository, and asserts the child
/// passed and left that repository's config as it was.
pub(crate) fn assert_hook_env_leaves_the_repository_alone(skip: &str) {
    let outer = tempfile::tempdir().unwrap();
    let outer_git = outer.path().join("outer.git");
    let init = git_command()
        .args(["init", "-q"])
        .arg(&outer_git)
        .output()
        .unwrap();
    assert!(init.status.success());
    let config = outer_git.join(".git/config");
    let before = std::fs::read_to_string(&config).unwrap();

    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--test-threads=1", "--skip", skip])
        .env(CHILD_MARKER, "1")
        .env("GIT_DIR", outer_git.join(".git"))
        .env("GIT_INDEX_FILE", outer_git.join(".git/index"))
        .env("GIT_WORK_TREE", outer.path().join("worktree"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the tests failed under a hook's git environment:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let after = std::fs::read_to_string(&config).unwrap();
    assert_eq!(before, after, "a test changed the surrounding repository");
}

/// Shells the guest scripts must work under (the guest's `/bin/sh` is dash on Debian).
pub(crate) fn shells() -> Vec<&'static str> {
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

/// Git for the fixture repositories, with auto maintenance off: newer git starts
/// `git maintenance run --auto` detached after a commit, which would still be working in the
/// repository while the script under test reads it.
pub(crate) fn git(dir: &Path, args: &[&str]) {
    let out = command("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "maintenance.auto")
        .env("GIT_CONFIG_VALUE_0", "false")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.org")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.org")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A volume root with: ext4's `lost+found`, Puddle's `.puddle`, a checkout with every kind of
/// unsaved work, a clean checkout, a repo without commits, and a stray file.
pub(crate) fn volume(root: &Path) -> PathBuf {
    let remote = root.join("remote.git");
    std::fs::create_dir(&remote).unwrap();
    git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    let vol = root.join("vol");
    std::fs::create_dir_all(vol.join("lost+found")).unwrap();
    std::fs::create_dir_all(vol.join(".puddle/code-server")).unwrap();
    std::fs::write(vol.join("notes.txt"), "loose\n").unwrap();

    let seed = root.join("seed");
    std::fs::create_dir(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]);
    std::fs::write(seed.join("a.txt"), "a\n").unwrap();
    git(&seed, &["add", "."]);
    git(&seed, &["commit", "-qm", "first"]);
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-q", "origin", "main"]);

    git(&vol, &["clone", "-q", remote.to_str().unwrap(), "web"]);
    git(&vol, &["clone", "-q", remote.to_str().unwrap(), "api"]);
    let api = vol.join("api");
    std::fs::write(api.join("a.txt"), "changed\n").unwrap();
    git(&api, &["stash", "-q"]);
    std::fs::write(api.join("b.txt"), "b\n").unwrap();
    git(&api, &["add", "b.txt"]);
    git(&api, &["commit", "-qm", "local only"]);
    std::fs::write(api.join("a.txt"), "edited\n").unwrap();
    std::fs::write(api.join("new.txt"), "untracked\n").unwrap();
    // A hostile repo config: the check must not run it.
    let pwned = root.join("pwned");
    git(
        &api,
        &[
            "config",
            "core.fsmonitor",
            &format!("touch {}; false", pwned.display()),
        ],
    );

    let fresh = vol.join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    git(&fresh, &["init", "-q"]);
    std::fs::write(fresh.join("x"), "x\n").unwrap();
    vol
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_git_variables_are_removed() {
        let cmd = command_without(
            "git",
            ["GIT_DIR", "GIT_INDEX_FILE", "PATH", "GIT_CONFIG_COUNT"].map(OsString::from),
        );
        let mut removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(removed, ["GIT_CONFIG_COUNT", "GIT_DIR", "GIT_INDEX_FILE"]);
    }
}
