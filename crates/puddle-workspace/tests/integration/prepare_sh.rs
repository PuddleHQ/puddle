// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real `guest/prepare.sh` under dash and bash, against a temp dir that plays the volume root.
//! A test is not root, so `chown` is a stub (`PUDDLE_CHOWN`) that records its arguments: the
//! script's own choices (what to give away, in which order, what to leave) are what is checked.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use puddle_workspace::PREPARE_SH;

use crate::common::{command, shells};

struct Fixture {
    _tmp: tempfile::TempDir,
    vol: PathBuf,
    chown: PathBuf,
    log: PathBuf,
}

/// A volume with a checkout, `.puddle/editor`, `lost+found` with a file in it, and a symlink that
/// leads off the volume; plus the `chown` stub.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let vol = tmp.path().join("vol");
    std::fs::create_dir_all(vol.join("api/src")).unwrap();
    std::fs::write(vol.join("api/src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir_all(vol.join(".puddle/editor")).unwrap();
    std::fs::write(vol.join(".puddle/editor/acme.code-workspace"), "{}").unwrap();
    std::fs::create_dir_all(vol.join("lost+found")).unwrap();
    std::fs::write(vol.join("lost+found/#123"), "").unwrap();
    std::os::unix::fs::symlink(tmp.path(), vol.join("outside")).unwrap();
    let log = tmp.path().join("chown.log");
    let chown = tmp.path().join("chown");
    puddle_test_kit::write_executable(
        &chown,
        &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n", log.display()),
    );
    Fixture {
        _tmp: tmp,
        vol,
        chown,
        log,
    }
}

fn run(shell: &str, fx: &Fixture, vol: &Path, user: &str) -> Output {
    command(shell)
        .args(["-c", PREPARE_SH, "puddle-prepare"])
        .arg(vol)
        .arg(user)
        .env("PUDDLE_CHOWN", &fx.chown)
        .output()
        .unwrap()
}

/// What the stub was called with, one call per line.
fn calls(fx: &Fixture) -> Vec<String> {
    std::fs::read_to_string(&fx.log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn me() -> (String, String) {
    let text = |args: &[&str]| {
        let out = command("id").args(args).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    (
        text(&["-un"]),
        format!("{}:{}", text(&["-u"]), text(&["-g"])),
    )
}

#[test]
fn a_volume_the_user_already_owns_only_gets_puddle_and_the_editor_files() {
    let (user, owner) = me();
    for shell in shells() {
        let fx = fixture();
        std::fs::remove_dir_all(fx.vol.join(".puddle")).unwrap();
        let out = run(shell, &fx, &fx.vol, &user);
        assert!(out.status.success(), "{shell}: {out:?}");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "D\n", "{shell}");
        let mode = std::fs::metadata(fx.vol.join(".puddle"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{shell}");
        let v = fx.vol.display();
        assert_eq!(
            calls(&fx),
            [format!("-h {owner} {v}/.puddle {v}")],
            "{shell}: no editor folder, nothing else touched"
        );

        // With the editor folder the boot hook writes as root, that is given away too.
        std::fs::create_dir_all(fx.vol.join(".puddle/editor")).unwrap();
        std::fs::remove_file(&fx.log).unwrap();
        let out = run(shell, &fx, &fx.vol, &user);
        assert!(out.status.success(), "{shell}: {out:?}");
        assert_eq!(
            calls(&fx),
            [
                format!("-R -h {owner} {v}/.puddle/editor"),
                format!("-h {owner} {v}/.puddle {v}")
            ],
            "{shell}"
        );
    }
}

#[test]
fn a_volume_of_another_owner_is_given_whole_to_the_user_root_last_and_lost_found_kept() {
    // The fixture belongs to the test's user, so `root` is "another owner" here.
    for shell in shells() {
        let fx = fixture();
        let out = run(shell, &fx, &fx.vol, "root");
        assert!(out.status.success(), "{shell}: {out:?}");
        let v = fx.vol.display();
        // api, api/src, api/src/main.rs, .puddle, .puddle/editor, its file, outside.
        assert_eq!(String::from_utf8_lossy(&out.stdout), "R\t7\nD\n", "{shell}");
        let calls = calls(&fx);
        assert_eq!(calls.len(), 3, "{shell}: {calls:?}");
        let mut given: Vec<&str> = calls[0]
            .strip_prefix("-h 0:0 ")
            .unwrap()
            .split(' ')
            .collect();
        given.sort_unstable();
        let mut want = [
            "api",
            "api/src",
            "api/src/main.rs",
            ".puddle",
            ".puddle/editor",
            ".puddle/editor/acme.code-workspace",
            "outside",
        ]
        .map(|p| format!("{v}/{p}"));
        want.sort_unstable();
        assert_eq!(
            given, want,
            "{shell}: lost+found and the root are not in the sweep"
        );
        assert_eq!(calls[1], format!("-R -h 0:0 {v}/.puddle/editor"), "{shell}");
        assert_eq!(
            calls[2],
            format!("-h 0:0 {v}/.puddle {v}"),
            "{shell}: the root goes last"
        );
    }
}

#[test]
fn an_unknown_user_or_a_missing_volume_fails_with_the_reason() {
    let (user, _) = me();
    for shell in shells() {
        let fx = fixture();
        let out = run(shell, &fx, &fx.vol, "no-such-user-puddle");
        assert_eq!(out.status.code(), Some(3), "{shell}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("has no user no-such-user-puddle"),
            "{shell}: {out:?}"
        );
        let out = run(shell, &fx, &fx.vol.join("gone"), &user);
        assert_eq!(out.status.code(), Some(3), "{shell}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("no volume at"),
            "{shell}: {out:?}"
        );
        assert!(calls(&fx).is_empty(), "{shell}");
    }
}
