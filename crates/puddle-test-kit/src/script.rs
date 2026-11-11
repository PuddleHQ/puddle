// SPDX-License-Identifier: GPL-3.0-or-later
//! A script a test can run straight after writing it.

use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Writes `contents` to `path` and makes it executable (mode 0755), so the caller can execute it
/// at once without `ETXTBSY` ("Text file busy").
///
/// The file is written by a short-lived `sh` child, not by this process. A write from here keeps a
/// writable descriptor open for a moment; a sibling test thread that forks in that moment copies
/// the descriptor into its child, which holds it until its own `exec`, and an `exec` of the file
/// during that time fails with `ETXTBSY`. The child's descriptor is closed when it exits, and this
/// process never holds one, so no fork can copy it.
///
/// # Panics
/// When `sh` cannot run or fails to write the file.
pub fn write_executable(path: &Path, contents: &str) {
    let status = Command::new("sh")
        .arg("-c")
        .arg(r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#)
        .arg("sh")
        .arg(contents)
        .arg(path)
        .status()
        .expect("run sh to write a test script");
    assert!(
        status.success(),
        "sh failed to write the script {}: {status}",
        path.display()
    );
}

/// Like [`write_executable`] for contents that are not text (a stand-in binary): the bytes reach
/// the file through the stdin of a short-lived `sh` child, so this process never holds a writable
/// descriptor on it either.
///
/// # Panics
/// When `sh` cannot run or fails to write the file.
pub fn write_executable_bytes(path: &Path, contents: &[u8]) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(r#"cat > "$1" && chmod 755 "$1""#)
        .arg("sh")
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("run sh to write a test program");
    feed_stdin(&mut child, contents);
    let status = child.wait().expect("wait for sh");
    assert!(
        status.success(),
        "sh failed to write the program {}: {status}",
        path.display()
    );
}

/// Sends `bytes` to the child's stdin and closes it.
///
/// A child can exit before it reads (a script that fails on its arguments first), which closes the
/// pipe and fails the write with `BrokenPipe`. That is the child's outcome, which the caller reads
/// from its exit status and output, so it is not an error here; any other write error panics.
///
/// # Panics
/// When the child has no piped stdin, or the write fails for a reason other than a closed pipe.
pub fn feed_stdin(child: &mut Child, bytes: &[u8]) {
    let mut stdin = child.stdin.take().expect("piped stdin");
    if let Err(e) = stdin.write_all(bytes) {
        assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "send the input to the child: {e}"
        );
    }
}

#[cfg(test)]
mod tests {
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let kind = loop {
            if let Err(e) = held.write_all(b"x") {
                break e.kind();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the pipe never closed"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        };
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
}
