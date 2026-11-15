// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration test: `puddle-agent mise-fetch` as the host runs it, against the real pin. The real
//! download is the VM test's; here `curl` is a script, so the binary's own checks (the pinned
//! hash, the marker, the exit codes the host reads) run without a network.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns, only in tests"
)]

use std::path::Path;
use std::process::{Command, Output};

use puddle_agent::mise::Pin;
use sha2::{Digest as _, Sha256};

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Runs the subcommand with `path` as the whole `PATH`.
fn mise_fetch(dir: &Path, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_puddle-agent"))
        .arg("mise-fetch")
        .arg(dir)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("puddle-agent-mise-")
        .tempdir()
        .unwrap()
}

#[test]
fn a_binary_that_matches_the_marker_is_current_and_downloads_nothing() {
    let tmp = tmp();
    let dir = tmp.path().join("mise");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/mise"), b"the binary").unwrap();
    let pin = Pin::current().unwrap();
    std::fs::write(
        dir.join("bin/.puddle-mise"),
        format!("{} {} {}\n", pin.version, pin.sha256, hex(b"the binary")),
    )
    .unwrap();
    // No curl and no wget on this PATH: a download would fail, so success proves there was none.
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let out = mise_fetch(&dir, &empty);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "current\n");
}

#[test]
fn a_download_that_is_not_the_pinned_file_is_refused_with_the_reason_on_standard_error() {
    let tmp = tmp();
    let dir = tmp.path().join("mise");
    let bin = tmp.path().join("fake-bin");
    std::fs::create_dir(&bin).unwrap();
    let curl = bin.join("curl");
    // Writes something else to the `--output` file.
    puddle_test_kit::write_executable(
        &curl,
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output ] && out=$2; shift; done\nprintf 'not mise' > \"$out\"\n",
    );
    let out = mise_fetch(&dir, &bin);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "the mise download does not match the checksum Puddle expects\n"
    );
    assert_eq!(out.stdout, b"");
    assert!(!dir.join("bin/mise").exists());
    assert!(
        !dir.join(".mise-download.tar.gz").exists(),
        "no tarball is kept"
    );
}

#[test]
fn an_image_without_curl_or_wget_says_so() {
    let tmp = tmp();
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let out = mise_fetch(&tmp.path().join("mise"), &empty);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "the image has neither curl nor wget, which Puddle needs to download mise\n"
    );
}

#[test]
fn a_folder_that_cannot_be_made_is_reported() {
    let tmp = tmp();
    let file = tmp.path().join("a-file");
    std::fs::write(&file, b"").unwrap();
    let out = mise_fetch(&file.join("mise"), tmp.path());
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .starts_with("could not write mise to the workspace volume: "),
        "{out:?}"
    );
}

#[test]
fn without_a_folder_it_prints_the_usage_and_exits_2() {
    let out = Command::new(env!("CARGO_BIN_EXE_puddle-agent"))
        .arg("mise-fetch")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: puddle-agent mise-fetch <dir>"));
}
