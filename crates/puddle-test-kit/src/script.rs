// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
