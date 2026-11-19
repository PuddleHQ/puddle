// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `guest/clone.sh` under dash and bash, with the real `git`, against a temp dir that plays
//! the volume root and a local repository that plays the remote.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};
use std::process::Output;

use puddle_workspace::CLONE_SH;

use crate::common::{command, shells};

struct Fixture {
    _tmp: tempfile::TempDir,
    /// The repository the clone reads, as a `file://` URL.
    url: String,
    vol: PathBuf,
}

impl Fixture {
    fn stage(&self) -> PathBuf {
        self.vol.join(".puddle/cloning/api")
    }

    fn checkout(&self) -> PathBuf {
        self.vol.join("api")
    }
}

/// A remote with one commit and an empty volume.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "-q", "-b", "main"]);
    std::fs::write(remote.join("README"), "hello\n").unwrap();
    git(&remote, &["add", "README"]);
    git(&remote, &["commit", "-qm", "first"]);
    let vol = tmp.path().join("vol");
    std::fs::create_dir_all(vol.join(".puddle")).unwrap();
    Fixture {
        url: format!("file://{}", remote.display()),
        _tmp: tmp,
        vol,
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = command("git")
        .args([
            "-c",
            "user.name=Puddle Test",
            "-c",
            "user.email=test@puddle.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

fn clone(shell: &str, fx: &Fixture, url: &str) -> Output {
    command(shell)
        .args(["-c", CLONE_SH, "puddle-clone", url])
        .arg(fx.stage())
        .arg(fx.checkout())
        .output()
        .unwrap()
}

/// What a clone the VM's death cut short leaves in the stage: a repository git had begun.
fn leave_a_dead_clone(fx: &Fixture) {
    std::fs::create_dir_all(fx.stage().join(".git/objects/pack")).unwrap();
    std::fs::write(fx.stage().join(".git/objects/pack/tmp_pack_x"), b"half").unwrap();
    std::fs::write(fx.stage().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
}

#[test]
fn a_clone_lands_whole_in_the_checkout_and_leaves_no_stage() {
    for shell in shells() {
        let fx = fixture();
        let out = clone(shell, &fx, &fx.url);
        assert!(out.status.success(), "{shell}: {out:?}");
        assert_eq!(
            std::fs::read_to_string(fx.checkout().join("README")).unwrap(),
            "hello\n",
            "{shell}"
        );
        assert!(!fx.stage().exists(), "{shell}: the stage is left");
    }
}

#[test]
fn a_clone_after_one_that_died_removes_what_it_left_and_works() {
    for shell in shells() {
        let fx = fixture();
        leave_a_dead_clone(&fx);
        let out = clone(shell, &fx, &fx.url);
        assert!(out.status.success(), "{shell}: {out:?}");
        assert!(fx.checkout().join("README").is_file(), "{shell}");
        assert!(!fx.stage().exists(), "{shell}");
        let fsck = command("git")
            .args(["fsck", "--full"])
            .current_dir(fx.checkout())
            .output()
            .unwrap();
        assert!(fsck.status.success(), "{shell}: {fsck:?}");
    }
}

#[test]
fn a_checkout_that_exists_is_never_cloned_into_and_keeps_its_files() {
    for shell in shells() {
        let fx = fixture();
        std::fs::create_dir_all(fx.checkout()).unwrap();
        std::fs::write(fx.checkout().join("mine.txt"), "work\n").unwrap();
        let out = clone(shell, &fx, &fx.url);
        assert_eq!(out.status.code(), Some(3), "{shell}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("never clones into a folder"),
            "{shell}: {out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(fx.checkout().join("mine.txt")).unwrap(),
            "work\n"
        );
        assert!(!fx.checkout().join("README").exists(), "{shell}");
    }
}

#[test]
fn a_failed_clone_exits_with_gits_code_and_leaves_the_checkouts_place_empty() {
    for shell in shells() {
        let fx = fixture();
        let missing = format!("{}-gone", fx.url);
        let out = clone(shell, &fx, &missing);
        assert_eq!(out.status.code(), Some(128), "{shell}: {out:?}");
        assert!(!fx.checkout().exists(), "{shell}");
    }
}
