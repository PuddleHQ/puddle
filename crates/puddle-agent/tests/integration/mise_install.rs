// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration test: `puddle-agent mise-install` as the host runs it. The workspace's mise is a
//! script whose state lives in marker files, so the report, the exit code and the last error
//! line the host reads come from the real binary without a network.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns, only in tests"
)]

use std::path::Path;
use std::process::{Command, Output};

/// A workspace whose root asks for node, with a stand-in mise at the pinned place. `ls` reports
/// node missing until `installed` exists; `install` creates it unless `install-fails` exists, and
/// with `install-forgets` it ends well without installing; with `ls-breaks`, `ls` fails once
/// node is in.
fn workspace() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("puddle-agent-mise-install-")
        .tempdir()
        .unwrap();
    let dir = root.path().display();
    std::fs::write(
        root.path().join("mise.toml"),
        "[tools]\nnode = \"22.11.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join(".puddle/mise/bin")).unwrap();
    puddle_test_kit::write_executable(
        &root.path().join(".puddle/mise/bin/mise"),
        &format!(
            "#!/bin/sh\nstate={dir}\ncase \"$1\" in\n  ls)\n    if [ -f \"$state/installed\" ]; then\n      [ -f \"$state/ls-breaks\" ] && {{ echo 'mise ERROR mise.toml changed while reading' >&2; exit 1; }}\n      echo '{{}}'\n    else\n      echo '{{\"node\":[{{\"version\":\"22.11.0\"}}]}}'\n    fi ;;\n  install)\n    [ -f \"$state/install-fails\" ] && {{ echo 'mise ERROR no route to nodejs.org' >&2; exit 1; }}\n    [ -f \"$state/install-forgets\" ] || touch \"$state/installed\" ;;\nesac\n"
        ),
    );
    root
}

fn mise_install(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_puddle-agent"))
        .arg("mise-install")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_good_install_reports_what_was_missing_and_landed() {
    let root = workspace();
    let report = mise_install(root.path(), &["--missing"]);
    assert!(report.status.success(), "{report:?}");
    assert_eq!(text(&report.stdout), "missing node@22.11.0\nend\n");
    assert!(
        !root.path().join("installed").exists(),
        "a report installs nothing"
    );

    let out = mise_install(root.path(), &[]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "missing node@22.11.0\ninstalled node@22.11.0\nend\n"
    );
    let again = mise_install(root.path(), &[]);
    assert!(again.status.success(), "{again:?}");
    assert_eq!(text(&again.stdout), "end\n");
}

#[test]
fn a_tool_still_missing_after_a_quiet_install_fails_and_is_named() {
    let root = workspace();
    std::fs::write(root.path().join("install-forgets"), "").unwrap();
    let out = mise_install(root.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stdout),
        "missing node@22.11.0\nfailed node@22.11.0\nend\n"
    );
    assert_eq!(text(&out.stderr), "mise did not install node@22.11.0\n");
}

#[test]
fn mises_own_error_is_the_last_line_on_standard_error() {
    let root = workspace();
    std::fs::write(root.path().join("install-fails"), "").unwrap();
    let out = mise_install(root.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr).lines().last(),
        Some("mise ERROR no route to nodejs.org")
    );
    assert!(text(&out.stdout).contains("failed node@22.11.0"), "{out:?}");
}

#[test]
fn a_mise_that_breaks_after_the_install_is_quoted() {
    let root = workspace();
    std::fs::write(root.path().join("ls-breaks"), "").unwrap();
    let out = mise_install(root.path(), &[]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        text(&out.stderr).lines().last(),
        Some("mise ERROR mise.toml changed while reading")
    );
}
