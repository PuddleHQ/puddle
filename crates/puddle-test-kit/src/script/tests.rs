// SPDX-License-Identifier: AGPL-3.0-or-later
use std::os::unix::fs::PermissionsExt as _;
use std::thread;

use super::*;

#[test]
fn the_bytes_version_keeps_nul_and_high_bytes_and_is_executable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prog");
    let bytes: Vec<u8> = (0..=255).chain(0..=255).collect();
    write_executable_bytes(&path, &bytes);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn the_file_holds_the_exact_text_and_is_executable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool");
    let text = "#!/bin/sh\necho 'it''s' \"$1\" %s \\n $HOME\n";
    write_executable(&path, text);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let out = Command::new(&path).arg("x").output().unwrap();
    assert!(out.status.success());
}

#[test]
fn a_path_with_spaces_and_a_replaced_file_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a b $c");
    write_executable(&path, "#!/bin/sh\necho one\n");
    write_executable(&path, "#!/bin/sh\necho two\n");
    let out = Command::new(&path).output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "two\n");
}

#[test]
#[should_panic(expected = "sh failed to write the script")]
fn a_folder_that_does_not_exist_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    write_executable(&dir.path().join("missing").join("tool"), "#!/bin/sh\n");
}

#[test]
fn a_child_that_exits_without_reading_is_not_a_write_error() {
    let mut child = Command::new("true").stdin(Stdio::piped()).spawn().unwrap();
    let mut held = child.stdin.take().unwrap();
    child.wait().unwrap();
    // The pipe's read end is gone, so a write is the `BrokenPipe` case itself. A child another
    // test is spawning at this moment can inherit the read end for the instant before its exec
    // closes it, and a write then lands in the pipe; so write until the pipe reports closed.
    let kind = (0..2000)
        .find_map(|_| {
            thread::sleep(std::time::Duration::from_millis(5));
            // flake-ok: this test is the BrokenPipe case itself; the result is read as data and retried until the pipe closes, never unwrapped.
            held.write_all(b"x").err().map(|e| e.kind())
        })
        .expect("the pipe never closed");
    assert_eq!(kind, std::io::ErrorKind::BrokenPipe);
    child.stdin = Some(held);
    feed_stdin(&mut child, b"never read");
}

#[test]
fn input_the_child_reads_arrives_whole() {
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    feed_stdin(&mut child, b"all of it");
    assert_eq!(child.wait_with_output().unwrap().stdout, b"all of it");
}

#[test]
#[should_panic(expected = "sh failed to write the program")]
fn a_folder_that_does_not_exist_fails_loudly_for_bytes_too() {
    let dir = tempfile::tempdir().unwrap();
    write_executable_bytes(&dir.path().join("missing").join("prog"), b"x");
}

/// The race itself: threads that write then run a script while their siblings fork constantly.
#[test]
fn running_a_script_right_after_writing_it_never_hits_text_file_busy() {
    let threads: Vec<_> = (0..8)
        .map(|n| {
            thread::spawn(move || {
                let dir = tempfile::tempdir().unwrap();
                for i in 0..60 {
                    let path = dir.path().join(format!("s{n}-{i}"));
                    write_executable(&path, "#!/bin/sh\nexit 0\n");
                    let status = Command::new(&path)
                        .status()
                        .unwrap_or_else(|e| panic!("run script {n}-{i}: {e}"));
                    assert!(status.success());
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}
