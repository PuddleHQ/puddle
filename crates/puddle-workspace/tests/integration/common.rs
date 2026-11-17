// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the host-script tests share on top of [`crate::git_env`]: the hook-environment regression, the
//! shells the guest scripts run under, and the fixture repositories and volume.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

#[cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(unix)]
pub(crate) use crate::git_env::command;
use crate::git_env::git_command;

/// Set in the child run of [`assert_hook_env_leaves_the_repository_alone`].
const CHILD_MARKER: &str = "PUDDLE_TEST_HOOK_ENV_CHILD";

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
        .args(["--test-threads=4", "--skip", skip])
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
#[cfg(unix)]
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
#[cfg(unix)]
pub(crate) fn git(dir: &Path, args: &[&str]) {
    git_output(dir, args);
}

/// [`git`], returning what it printed.
#[cfg(unix)]
pub(crate) fn git_output(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A volume root with: ext4's `lost+found`, Puddle's `.puddle`, a checkout with every kind of
/// unsaved work, a clean checkout, a repo without commits, and a stray file.
#[cfg(unix)]
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

/// Every way a checkout's own config or attributes could make `git status` or `git log` start a
/// program, by the name of the marker file that program leaves.
#[cfg(unix)]
pub(crate) const HOSTILE_COMMANDS: [&str; 5] = [
    "clean-filter",
    "process-filter",
    "index-hook",
    "signature-check",
    "submodule-filter",
];

/// Makes `vol/evil`: a checkout whose config names a program for each of [`HOSTILE_COMMANDS`]
/// (a clean filter from `.gitattributes`, a process filter from `.git/info/attributes`, a
/// `post-index-change` hook, a `gpg.program` with `log.showSignature` and a signed commit, and a
/// clean filter in the config of a repository nested in it), with files whose content is
/// unchanged but whose time is not, so git has to run the filters to compare them. It also has
/// one untracked file, `new.txt`. Each program writes its marker in the returned folder.
#[cfg(unix)]
pub(crate) fn hostile_checkout(root: &Path, vol: &Path) -> PathBuf {
    let markers = root.join("ran");
    let bin = root.join("bin");
    std::fs::create_dir_all(&markers).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let script = |name: &str| {
        format!(
            "#!/bin/sh\n: > '{}'\nexec cat\n",
            markers.join(name).display()
        )
    };
    for name in HOSTILE_COMMANDS {
        puddle_test_kit::write_executable(&bin.join(name), &script(name));
    }
    let program = |name: &str| bin.join(name).display().to_string();
    let touch_old = |file: &Path| {
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options()
            .write(true)
            .open(file)
            .unwrap()
            .set_modified(old)
            .unwrap();
    };

    let evil = vol.join("evil");
    std::fs::create_dir_all(&evil).unwrap();
    git(&evil, &["init", "-q", "-b", "main"]);
    std::fs::write(evil.join(".gitattributes"), "*.c filter=evil\n").unwrap();
    std::fs::write(evil.join("a.c"), "c\n").unwrap();
    std::fs::write(evil.join("a.p"), "p\n").unwrap();

    let inner = evil.join("inner");
    std::fs::create_dir(&inner).unwrap();
    git(&inner, &["init", "-q", "-b", "main"]);
    std::fs::write(inner.join(".gitattributes"), "*.s filter=nested\n").unwrap();
    std::fs::write(inner.join("x.s"), "s\n").unwrap();
    git(&inner, &["add", "."]);
    git(&inner, &["commit", "-qm", "inner"]);
    git(&evil, &["add", ".gitattributes", "a.c", "a.p", "inner"]);
    git(&evil, &["commit", "-qm", "first"]);

    // A signed commit on top: `log.showSignature` hands its signature to `gpg.program`.
    let tree = git_output(&evil, &["rev-parse", "HEAD^{tree}"]);
    let parent = git_output(&evil, &["rev-parse", "HEAD"]);
    let commit = root.join("signed-commit");
    std::fs::write(
        &commit,
        format!(
            "tree {tree}\nparent {parent}\nauthor T <t@example.org> 1700000000 +0000\n\
             committer T <t@example.org> 1700000000 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEzBAABCAAdFiEE\n -----END PGP SIGNATURE-----\n\
             \nsigned\n"
        ),
    )
    .unwrap();
    let signed = git_output(
        &evil,
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            commit.to_str().unwrap(),
        ],
    );
    git(&evil, &["update-ref", "refs/heads/main", &signed]);

    std::fs::write(evil.join(".git/info/attributes"), "*.p filter=info\n").unwrap();
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    puddle_test_kit::write_executable(&hooks.join("post-index-change"), &script("index-hook"));
    for (key, value) in [
        ("filter.evil.clean", program("clean-filter")),
        ("filter.evil.required", "true".to_owned()),
        ("filter.info.process", program("process-filter")),
        ("core.hooksPath", hooks.display().to_string()),
        ("log.showSignature", "true".to_owned()),
        ("gpg.program", program("signature-check")),
    ] {
        git(&evil, &["config", key, &value]);
    }
    git(
        &inner,
        &[
            "config",
            "filter.nested.clean",
            &program("submodule-filter"),
        ],
    );
    for file in [evil.join("a.c"), evil.join("a.p"), inner.join("x.s")] {
        touch_old(&file);
    }
    std::fs::write(evil.join("new.txt"), "untracked\n").unwrap();
    markers
}

/// The [`HOSTILE_COMMANDS`] that left a marker in `markers`.
#[cfg(unix)]
pub(crate) fn hostile_commands_that_ran(markers: &Path) -> Vec<String> {
    let mut ran: Vec<String> = std::fs::read_dir(markers)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    ran.sort();
    ran
}
