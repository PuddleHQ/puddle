// SPDX-License-Identifier: GPL-3.0-or-later
//! Downloading in the guest: the guest's own `curl` (or `wget`), a SHA-256 check, nothing else.
//! The VS Code server ([`crate::microsoft`]) and the pinned mise ([`crate::mise`]) share it, so
//! both go through Puddle's proxy and trust store the same way.

use std::ffi::OsString;
use std::future::Future;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use sha2::{Digest as _, Sha256};
use tokio::process::Command;

/// Longest answer of the update service that is accepted (the real ones are a few hundred bytes).
/// A longer one is an error and the read stops there; the rest is never buffered.
const MAX_ANSWER: usize = 64 * 1024;
/// How long one metadata request may take.
const ANSWER_SECS: u32 = 60;
/// A download that moves under 1 KiB/s for this long is given up on; its total time is not capped
/// (the tarball is a few hundred MB, over any kind of link).
const STALL_SECS: u32 = 60;

/// How the agent reads a small answer and downloads a file.
pub trait Fetch: Send + Sync + 'static {
    /// The body of `url` as text (a few hundred bytes).
    fn text(&self, url: &str) -> impl Future<Output = io::Result<String>> + Send;

    /// Downloads `url` to the file `dest`.
    fn file(&self, url: &str, dest: &Path) -> impl Future<Output = io::Result<()>> + Send;
}

/// [`Fetch`] with the guest's own `curl`, or `wget` where it has none. HTTPS only, redirects
/// included.
#[derive(Debug, Clone)]
pub struct Downloader {
    curl: PathBuf,
    wget: PathBuf,
}

impl Downloader {
    /// The guest's `curl` and `wget`, found on its `PATH`.
    #[must_use]
    pub fn system() -> Self {
        Self::with_programs("curl", "wget")
    }

    /// A downloader that runs these programs (tests use scripts).
    #[must_use]
    pub fn with_programs(curl: impl Into<PathBuf>, wget: impl Into<PathBuf>) -> Self {
        Self {
            curl: curl.into(),
            wget: wget.into(),
        }
    }

    fn curl_args(url: &str, secs: u32, dest: Option<&Path>, limit: Option<usize>) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
        ]
        .map(OsString::from)
        .into();
        if dest.is_some() {
            // A file: only a stalled transfer ends it.
            args.extend(["--speed-limit", "1024", "--speed-time"].map(OsString::from));
            args.push(STALL_SECS.to_string().into());
        } else {
            args.push("--max-time".into());
            args.push(secs.to_string().into());
        }
        if let Some(limit) = limit {
            // Refuses a body that announces more; the bounded read in `run` covers one that does
            // not (chunked, or a lying length).
            args.push("--max-filesize".into());
            args.push(limit.to_string().into());
        }
        if let Some(dest) = dest {
            args.push("--output".into());
            args.push(dest.into());
        }
        args.push(url.into());
        args
    }

    fn wget_args(url: &str, secs: u32, dest: Option<&Path>) -> Vec<OsString> {
        let mut args: Vec<OsString> = ["--quiet", "--https-only", "--timeout"]
            .map(OsString::from)
            .into();
        args.push(secs.to_string().into());
        args.push("--output-document".into());
        args.push(dest.map_or_else(|| OsString::from("-"), Into::into));
        args.push(url.into());
        args
    }

    /// Runs `curl`, else `wget` when there is no `curl`, and returns the standard output. With a
    /// `limit`, no more than that many bytes are read: a longer body kills the program and is an
    /// error.
    async fn run(
        &self,
        url: &str,
        secs: u32,
        dest: Option<&Path>,
        limit: Option<usize>,
    ) -> io::Result<Vec<u8>> {
        let attempt = [
            (&self.curl, Self::curl_args(url, secs, dest, limit)),
            (&self.wget, Self::wget_args(url, secs, dest)),
        ];
        for (program, args) in attempt {
            let child = Command::new(program)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let child = match child {
                Ok(child) => child,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            let (status, stdout, stderr) = read_bounded(child, limit).await?;
            if stdout.len() > limit.unwrap_or(usize::MAX) {
                return Err(io::Error::other(format!(
                    "{} sent more than {} bytes; the answer is refused",
                    program.display(),
                    limit.unwrap_or(0)
                )));
            }
            if status.success() {
                return Ok(stdout);
            }
            let said = String::from_utf8_lossy(&stderr);
            return Err(io::Error::other(format!(
                "{} failed ({status}): {}",
                program.display(),
                said.trim()
            )));
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the image has neither curl nor wget, which downloads in the guest need",
        ))
    }
}

/// Waits for `child`, reading its output. With a `limit`, standard output is read up to one byte
/// past it; then the child is killed instead of waited for, and the caller sees the excess.
async fn read_bounded(
    mut child: tokio::process::Child,
    limit: Option<usize>,
) -> io::Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    use tokio::io::{AsyncReadExt as _, BufReader};

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no standard output"))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no standard error"))?;
    let cap = limit.map_or(u64::MAX, |l| l as u64 + 1);
    let mut out = Vec::new();
    let mut err = Vec::new();
    let read_stdout = async {
        let read = BufReader::new(stdout).take(cap).read_to_end(&mut out).await;
        if out.len() as u64 >= cap {
            // Stop the program now (it may send without end); that also closes its stderr.
            let _ = child.start_kill();
        }
        read
    };
    let (read_out, read_err) = tokio::join!(read_stdout, stderr.read_to_end(&mut err));
    read_out?;
    read_err?;
    let status = child.wait().await?;
    Ok((status, out, err))
}

impl Fetch for Downloader {
    async fn text(&self, url: &str) -> io::Result<String> {
        let body = self.run(url, ANSWER_SECS, None, Some(MAX_ANSWER)).await?;
        String::from_utf8(body).map_err(|_| io::Error::other("the answer is not text"))
    }

    async fn file(&self, url: &str, dest: &Path) -> io::Result<()> {
        self.run(url, STALL_SECS, Some(dest), None).await.map(drop)
    }
}

/// The SHA-256 of the file at `path`, as lower-case hex.
///
/// # Errors
///
/// When the file cannot be read.
pub fn sha256_of(path: &Path) -> io::Result<String> {
    use std::fmt::Write as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut chunk = vec![0_u8; 1 << 16];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hasher.update(chunk.get(..n).unwrap_or_default());
    }
    Ok(hasher.finalize().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String can't fail
        s
    }))
}
