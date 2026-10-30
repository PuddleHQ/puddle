// SPDX-License-Identifier: GPL-3.0-or-later
//! Microsoft's VS Code server in the guest: found, downloaded, checked, unpacked on the workspace
//! volume and started directly. The host asks for it only after the user's consent
//! (`puddle_agent_proto::editor::ServerKind::Microsoft`); this module never decides that.
//!
//! The agent
//!
//! - asks Microsoft's update service for the newest `server-linux-<arch>-web` build, and for that
//!   build's SHA-256 (the `latest` answer carries none; `versions/<number>` does);
//! - downloads the tarball from the same service (through Puddle's proxy, like every guest
//!   connection) and refuses one whose SHA-256 differs from the one that service gave. That is an
//!   integrity check (it catches a corrupt or cut-short transfer), not authenticity: the hash and
//!   the tarball come from the same service over the same kind of connection, so a compromised
//!   service would pass it;
//! - unpacks it into `<builds>/<commit>` beside a marker file written last, so a half-unpacked
//!   build is never started, and remembers the newest complete one in `<builds>/current`;
//! - starts `bin/code-server` from it with the flags in [`arguments`]. It never runs `code
//!   serve-web`, never patches the files and keeps a build only on this workspace's volume.
//!
//! Resolving and downloading go through [`Fetch`], so the rest runs without a network in tests.
//! The real one ([`Downloader`]) runs `curl`, or `wget` where there is no `curl`: the guest's own
//! trust store and proxy variables apply to both, which is what makes the download pass through
//! Puddle's proxy.
//!
//! A build that is already unpacked is used as it is. Looking for a newer one is the update step
//! ([`stage_update`]): the new build is downloaded and verified into its own folder beside the
//! running one, which it never touches, and only *named* as staged. [`switch_to_staged`] makes it
//! the one to start (the caller restarts the server then, when no window is connected); the
//! previous build stays until the new one has accepted a connection once ([`confirm`]), and a new
//! build that exits before that is dropped for the previous one ([`roll_back`]). The server's
//! settings, extensions and secrets are in the data folder, not in a build, so they carry over.

use std::ffi::{OsStr, OsString};
use std::future::Future;
use std::io::{self, Read as _};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;

use flate2::read::GzDecoder;
use puddle_agent_proto::editor::EditorServer;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tar::EntryType;
use tokio::process::Command;

/// Microsoft's update service, the same one the `code` command uses.
pub const UPDATE_SERVICE: &str = "https://update.code.visualstudio.com";

/// Written into a build's folder after everything else: a folder without it is not a build.
const MARKER: &str = ".puddle-installed";
/// The file in the builds folder that names the build to start.
const CURRENT: &str = "current";
/// Names the build that ran before the current one and is kept until the current one is confirmed.
const PREVIOUS: &str = "previous";
/// Names the newest build that has accepted a connection.
const CONFIRMED: &str = "confirmed";
/// Names a downloaded, verified build that is waiting for a moment to switch to it.
const STAGED: &str = "staged";
/// Names a build that exited before it ever accepted a connection: it is not staged again.
const REJECTED: &str = "rejected";
/// The server's program, below a build's folder.
pub const PROGRAM: &str = "bin/code-server";
/// Longest answer of the update service that is accepted (the real ones are a few hundred bytes).
/// A longer one is an error and the read stops there; the rest is never buffered.
const MAX_ANSWER: usize = 64 * 1024;
/// How long one metadata request may take.
const ANSWER_SECS: u32 = 60;
/// A download that moves under 1 KiB/s for this long is given up on; its total time is not capped
/// (the tarball is a few hundred MB, over any kind of link).
const STALL_SECS: u32 = 60;
/// Where below the data folder the server keeps its own data.
const SERVER_DATA: &str = "server-data";
/// Where below the data folder its extensions live.
const EXTENSIONS: &str = "extensions";

/// Why a build could not be put on the volume.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    /// No server build exists for this machine's architecture.
    #[error("Microsoft publishes no VS Code server for the {0} architecture")]
    Unsupported(String),
    /// The update service could not be asked or answered something unusable.
    #[error("could not find the current VS Code server: {0}")]
    Resolve(String),
    /// The tarball could not be downloaded.
    #[error("could not download the VS Code server: {0}")]
    Download(String),
    /// The download does not match the SHA-256 the update service gave for it.
    #[error(
        "the downloaded VS Code server does not match the update service's SHA-256 ({actual}, expected {expected})"
    )]
    Checksum {
        /// What the update service gave.
        expected: String,
        /// What was downloaded.
        actual: String,
    },
    /// The tarball holds something that must not be unpacked.
    #[error("the VS Code server download holds {0}, which is not unpacked")]
    Archive(String),
    /// The volume could not be written.
    #[error("could not write the VS Code server to the workspace volume: {0}")]
    Io(#[from] io::Error),
}

impl InstallError {
    /// Whether a download was fetched and refused (it fails its SHA-256, or is not unpackable): the
    /// same build is usually refused again on every try, so retrying it soon is a waste of a download
    /// of hundreds of MB. A network that is down or an update service that is unreadable is not one.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Checksum { .. } | Self::Archive(_))
    }
}

/// The build name in the update service's path for a machine of architecture `arch` (the values
/// of `std::env::consts::ARCH`).
///
/// # Errors
///
/// [`InstallError::Unsupported`] for an architecture Microsoft builds no server for.
pub fn platform(arch: &str) -> Result<&'static str, InstallError> {
    match arch {
        "x86_64" => Ok("server-linux-x64-web"),
        "aarch64" => Ok("server-linux-arm64-web"),
        other => Err(InstallError::Unsupported(other.to_owned())),
    }
}

/// The newest build the update service knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Latest {
    /// Its commit: names the build's folder and its download.
    pub commit: String,
    /// Its release number, such as `1.143.1`.
    pub product_version: String,
}

#[derive(Deserialize)]
struct LatestReply {
    version: String,
    #[serde(rename = "productVersion")]
    product_version: String,
}

#[derive(Deserialize)]
struct VersionReply {
    version: String,
    sha256hash: String,
}

fn is_commit(text: &str) -> bool {
    text.len() == 40 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A release number as the service writes it (`1.143.1`, `1.144.0-insider`): it goes into a URL.
fn is_release(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 40
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// Reads the answer of [`latest_url`]. Everything that ends up in a path or a URL is checked.
///
/// # Errors
///
/// [`InstallError::Resolve`] for JSON that is not an answer or names a commit or release that
/// cannot be one.
pub fn parse_latest(json: &str) -> Result<Latest, InstallError> {
    let reply: LatestReply = serde_json::from_str(json).map_err(|e| {
        InstallError::Resolve(format!("the update service's answer is not usable: {e}"))
    })?;
    if !is_commit(&reply.version) || !is_release(&reply.product_version) {
        return Err(InstallError::Resolve(
            "the update service named a build with an unusable commit or release number".to_owned(),
        ));
    }
    Ok(Latest {
        commit: reply.version,
        product_version: reply.product_version,
    })
}

/// Reads the answer of [`versions_url`]: the SHA-256 the service gives of the build `commit`.
///
/// # Errors
///
/// [`InstallError::Resolve`] for JSON that is not an answer, one about another build than
/// `commit`, or a hash that is not 64 hex digits.
pub fn parse_published_hash(json: &str, commit: &str) -> Result<String, InstallError> {
    let reply: VersionReply = serde_json::from_str(json).map_err(|e| {
        InstallError::Resolve(format!("the update service's answer is not usable: {e}"))
    })?;
    if reply.version != commit {
        return Err(InstallError::Resolve(format!(
            "the update service answered for build {} when asked for {commit}",
            reply.version
        )));
    }
    let hash = reply.sha256hash.to_ascii_lowercase();
    if !is_sha256(&hash) {
        return Err(InstallError::Resolve(
            "the update service published no usable SHA-256 for the build".to_owned(),
        ));
    }
    Ok(hash)
}

/// Where the newest build is named.
#[must_use]
pub fn latest_url(platform: &str) -> String {
    format!("{UPDATE_SERVICE}/api/latest/{platform}/stable")
}

/// Where the build with release number `release` is described, with its SHA-256.
#[must_use]
pub fn versions_url(release: &str, platform: &str) -> String {
    format!("{UPDATE_SERVICE}/api/versions/{release}/{platform}/stable")
}

/// Where the tarball of the build `commit` is downloaded.
#[must_use]
pub fn download_url(commit: &str, platform: &str) -> String {
    format!("{UPDATE_SERVICE}/commit:{commit}/{platform}/stable")
}

/// How the agent reads the update service and downloads a build.
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
            "the image has neither curl nor wget, which the VS Code server download needs",
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

/// The commit `<builds>/<name>` holds, when it holds one.
fn pointer(builds: &Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(builds.join(name)).ok()?;
    let text = text.trim();
    is_commit(text).then(|| text.to_owned())
}

/// The folder of build `commit` when it is complete.
fn complete(builds: &Path, commit: &str) -> Option<PathBuf> {
    let dir = builds.join(commit);
    (dir.join(MARKER).is_file() && dir.join(PROGRAM).is_file()).then_some(dir)
}

/// The folder of the build `<builds>/current` names, when it is a complete build.
#[must_use]
pub fn current_build(builds: &Path) -> Option<PathBuf> {
    complete(builds, &pointer(builds, CURRENT)?)
}

/// The build to start: the one already on the volume, else the newest from Microsoft, put there.
///
/// # Errors
///
/// [`InstallError`], naming what failed, when there is no build and none could be installed.
pub async fn ensure_build(
    fetch: &impl Fetch,
    platform: &str,
    builds: &Path,
) -> Result<PathBuf, InstallError> {
    match current_build(builds) {
        Some(dir) => Ok(dir),
        None => install_latest(fetch, platform, builds).await,
    }
}

async fn resolve_latest(fetch: &impl Fetch, platform: &str) -> Result<Latest, InstallError> {
    let answer = fetch
        .text(&latest_url(platform))
        .await
        .map_err(|e| InstallError::Resolve(e.to_string()))?;
    parse_latest(&answer)
}

/// Puts the newest build on the volume (unless it is there) and makes it the current one.
///
/// # Errors
///
/// [`InstallError`]; a failed or refused download leaves no build behind, only what was there.
pub async fn install_latest(
    fetch: &impl Fetch,
    platform: &str,
    builds: &Path,
) -> Result<PathBuf, InstallError> {
    std::fs::create_dir_all(builds)?;
    let latest = resolve_latest(fetch, platform).await?;
    let dir = download_build(fetch, platform, builds, &latest).await?;
    write_pointer(builds, CURRENT, &latest.commit)?;
    Ok(dir)
}

/// Downloads, verifies and unpacks `latest` into its folder below `builds`, unless the folder is
/// already a complete build. Touches no pointer.
async fn download_build(
    fetch: &impl Fetch,
    platform: &str,
    builds: &Path,
    latest: &Latest,
) -> Result<PathBuf, InstallError> {
    let dir = builds.join(&latest.commit);
    if complete(builds, &latest.commit).is_none() {
        let answer = fetch
            .text(&versions_url(&latest.product_version, platform))
            .await
            .map_err(|e| InstallError::Resolve(e.to_string()))?;
        let expected = parse_published_hash(&answer, &latest.commit)?;
        let tarball = builds.join(format!("{}.tar.gz.partial", latest.commit));
        let staging = builds.join(format!("{}.partial", latest.commit));
        let fetched = fetch_and_unpack(
            fetch,
            &download_url(&latest.commit, platform),
            &expected,
            &tarball,
            &staging,
        )
        .await;
        // Whatever happened, the tarball (hundreds of MB) is not kept.
        let _ = std::fs::remove_file(&tarball);
        if let Err(err) = fetched {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
        // A folder without the marker is an earlier attempt that died: not a build, replaced whole
        // (a missing folder is the usual case, so the answer is not looked at).
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&staging, &dir)?;
    }
    Ok(dir)
}

/// What looking for a newer build found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// Microsoft's newest build is the one in use, or one that failed to start here before.
    UpToDate,
    /// A newer build is downloaded, verified and named as staged; it runs after
    /// [`switch_to_staged`] and a restart.
    Staged(String),
}

/// Asks Microsoft for the newest build. A newer one than the current is downloaded and verified
/// into its own folder, then named as staged; the running build's folder is not touched. Without
/// a current build there is nothing to update ([`ensure_build`] installs the first one).
///
/// # Errors
///
/// [`InstallError`]; a failed or refused download leaves nothing staged and the builds as they were.
pub async fn stage_update(
    fetch: &impl Fetch,
    platform: &str,
    builds: &Path,
) -> Result<Update, InstallError> {
    let current = pointer(builds, CURRENT);
    let latest = resolve_latest(fetch, platform).await?;
    let skip = current.as_deref() == Some(latest.commit.as_str())
        || pointer(builds, REJECTED).as_deref() == Some(latest.commit.as_str());
    if skip {
        return Ok(Update::UpToDate);
    }
    std::fs::create_dir_all(builds)?;
    download_build(fetch, platform, builds, &latest).await?;
    write_pointer(builds, STAGED, &latest.commit)?;
    Ok(Update::Staged(latest.commit))
}

/// Makes the staged build the one to start and keeps the one in use as `previous` until the new
/// one is confirmed. Returns the new commit, `None` when nothing is staged (a staged name whose
/// build is not complete is dropped). The server is not touched: the caller restarts it.
///
/// # Errors
///
/// When a pointer cannot be written; the old build then stays current.
pub fn switch_to_staged(builds: &Path) -> io::Result<Option<String>> {
    let Some(staged) = pointer(builds, STAGED) else {
        return Ok(None);
    };
    let _ = std::fs::remove_file(builds.join(STAGED));
    if complete(builds, &staged).is_none() {
        return Ok(None);
    }
    match pointer(builds, CURRENT) {
        Some(current) if current == staged => return Ok(None),
        Some(current) if complete(builds, &current).is_some() => {
            write_pointer(builds, PREVIOUS, &current)?;
        }
        _ => {}
    }
    write_pointer(builds, CURRENT, &staged)?;
    Ok(Some(staged))
}

/// Whether the current build has not yet run and the one it replaced is still there.
#[must_use]
pub fn on_trial(builds: &Path) -> bool {
    let current = pointer(builds, CURRENT);
    current.is_some()
        && current != pointer(builds, CONFIRMED)
        && pointer(builds, PREVIOUS).is_some_and(|p| complete(builds, &p).is_some())
}

/// The build `commit` has accepted a connection: it is the one to keep. The previous build and
/// everything else in the folder that is not a build in use goes, so the caller must not run this
/// while a download is going on (it would remove the download's folders) nor while a switch is
/// being made. Does nothing when `commit` is no longer the current build (a switch came first):
/// a build that never ran is never recorded as one that did.
///
/// # Errors
///
/// When the pointer cannot be written.
pub fn confirm(builds: &Path, commit: &str) -> io::Result<()> {
    let current = pointer(builds, CURRENT);
    if current.as_deref() != Some(commit) {
        return Ok(());
    }
    write_pointer(builds, CONFIRMED, commit)?;
    let _ = std::fs::remove_file(builds.join(PREVIOUS));
    prune(builds);
    Ok(())
}

/// The build `failed` exited before it ever accepted a connection: the previous one is current
/// again and the failed one is never staged again. `false` when `failed` is not the current build
/// on trial any more, or there was no previous build. Like [`confirm`], not to be run during a
/// download nor while a switch is being made.
///
/// # Errors
///
/// When a pointer cannot be written.
pub fn roll_back(builds: &Path, failed: &str) -> io::Result<bool> {
    let previous = pointer(builds, PREVIOUS)
        .filter(|_| on_trial(builds) && pointer(builds, CURRENT).as_deref() == Some(failed));
    let Some(previous) = previous else {
        return Ok(false);
    };
    write_pointer(builds, REJECTED, failed)?;
    write_pointer(builds, CURRENT, &previous)?;
    let _ = std::fs::remove_file(builds.join(PREVIOUS));
    prune(builds);
    Ok(true)
}

/// Deletes the build folders (and leftovers of dead downloads) that no pointer names. Anything
/// that is not named like one of those is left alone.
fn prune(builds: &Path) {
    let keep: Vec<String> = [CURRENT, PREVIOUS, STAGED]
        .into_iter()
        .filter_map(|name| pointer(builds, name))
        .collect();
    for entry in std::fs::read_dir(builds).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let ours = is_commit(name) || name.ends_with(".partial");
        if !ours || keep.iter().any(|k| k == name) {
            continue;
        }
        let path = entry.path();
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = gone {
            tracing::warn!(path = %path.display(), %err, "an old editor server build could not be removed");
        }
    }
}

async fn fetch_and_unpack(
    fetch: &impl Fetch,
    url: &str,
    expected: &str,
    tarball: &Path,
    staging: &Path,
) -> Result<(), InstallError> {
    fetch
        .file(url, tarball)
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    let (tar, out) = (tarball.to_owned(), staging.to_owned());
    let expected = expected.to_owned();
    tokio::task::spawn_blocking(move || {
        let actual = sha256_of(&tar)?;
        if actual != expected {
            return Err(InstallError::Checksum { expected, actual });
        }
        // Staging left by a dead attempt goes; usually there is none.
        let _ = std::fs::remove_dir_all(&out);
        unpack(&tar, &out)?;
        if !out.join(PROGRAM).is_file() {
            return Err(InstallError::Archive(format!("no {PROGRAM}")));
        }
        std::fs::write(out.join(MARKER), b"")?;
        Ok(())
    })
    .await
    .map_err(|e| io::Error::other(format!("the unpack task stopped: {e}")))?
}

/// The pointer file `name`, replaced whole.
fn write_pointer(builds: &Path, name: &str, commit: &str) -> io::Result<()> {
    let partial = builds.join(format!("{name}.partial"));
    std::fs::write(&partial, commit)?;
    std::fs::rename(&partial, builds.join(name))
}

fn sha256_of(path: &Path) -> io::Result<String> {
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

/// The path of an archive entry without the tarball's top folder, `None` for the folder itself.
fn below_top(path: &Path) -> Result<Option<PathBuf>, InstallError> {
    // A leading `./` is how some tools write every path; it is not a folder.
    let mut parts = path.components().filter(|c| *c != Component::CurDir);
    match parts.next() {
        Some(Component::Normal(_)) => {}
        None => return Ok(None),
        Some(_) => {
            return Err(InstallError::Archive(format!(
                "the path {}, which leaves the folder",
                path.display()
            )));
        }
    }
    let mut rest = PathBuf::new();
    for part in parts {
        match part {
            Component::Normal(name) => rest.push(name),
            _ => {
                return Err(InstallError::Archive(format!(
                    "the path {}, which leaves the folder",
                    path.display()
                )));
            }
        }
    }
    Ok((!rest.as_os_str().is_empty()).then_some(rest))
}

/// Unpacks the server tarball at `tarball` into `dest`, without its top folder. Only files and
/// folders are unpacked (Microsoft's tarball has no links; one in it is an error, since a link is
/// how a later entry could write outside `dest`).
fn unpack(tarball: &Path, dest: &Path) -> Result<(), InstallError> {
    let mut archive = tar::Archive::new(GzDecoder::new(std::fs::File::open(tarball)?));
    std::fs::create_dir_all(dest)?;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let Some(name) = below_top(&path)? else {
            continue;
        };
        let target = dest.join(&name);
        match entry.header().entry_type() {
            EntryType::Directory => std::fs::create_dir_all(&target)?,
            EntryType::Regular | EntryType::Continuous => {
                std::fs::create_dir_all(target.parent().unwrap_or(dest))?;
                let mut file = std::fs::File::create(&target)?;
                io::copy(&mut entry, &mut file)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // Owner read/write always; the other bits as published.
                    let mode = entry.header().mode().unwrap_or(0o644);
                    let kept = std::fs::Permissions::from_mode((mode & 0o755) | 0o600);
                    std::fs::set_permissions(&target, kept)?;
                }
            }
            other => {
                return Err(InstallError::Archive(format!(
                    "{} as a {other:?} entry",
                    name.display()
                )));
            }
        }
    }
    Ok(())
}

/// The server's command line (without the program). `target` is what [`crate::editor::target`]
/// says the editor opens.
#[must_use]
pub fn arguments(
    server: &EditorServer,
    telemetry: bool,
    token_file: &Path,
    target: &OsStr,
) -> Vec<OsString> {
    let data = Path::new(&server.data);
    let mut args: Vec<OsString> = vec![
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        server.port.to_string().into(),
        "--connection-token-file".into(),
        token_file.into(),
        "--accept-server-license-terms".into(),
        "--server-data-dir".into(),
        data.join(SERVER_DATA).into(),
        "--extensions-dir".into(),
        data.join(EXTENSIONS).into(),
        "--reconnection-grace-time".into(),
        server.grace_secs.to_string().into(),
    ];
    if !telemetry {
        args.push("--disable-telemetry".into());
    }
    args.push(
        if target == OsStr::new(&server.folder) {
            "--default-folder"
        } else {
            "--default-workspace"
        }
        .into(),
    );
    args.push(target.to_owned());
    args
}

/// Makes the folders the server keeps its data in; the server makes the rest.
///
/// # Errors
///
/// When a folder can't be made.
pub fn prepare(server: &EditorServer) -> io::Result<()> {
    for sub in [SERVER_DATA, EXTENSIONS] {
        std::fs::create_dir_all(Path::new(&server.data).join(sub))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "e646868227c3e3a9e3f24277c8c7220be42c0be7";

    #[test]
    fn the_urls_are_the_ones_the_code_command_uses() {
        assert_eq!(
            latest_url("server-linux-x64-web"),
            "https://update.code.visualstudio.com/api/latest/server-linux-x64-web/stable"
        );
        assert_eq!(
            versions_url("1.143.1", "server-linux-x64-web"),
            "https://update.code.visualstudio.com/api/versions/1.143.1/server-linux-x64-web/stable"
        );
        assert_eq!(
            download_url(COMMIT, "server-linux-arm64-web"),
            format!(
                "https://update.code.visualstudio.com/commit:{COMMIT}/server-linux-arm64-web/stable"
            )
        );
    }

    #[test]
    fn archive_paths_lose_their_top_folder_and_never_leave_the_build() {
        let below = |p: &str| below_top(Path::new(p)).map_err(|e| e.to_string());
        assert_eq!(
            below("top/bin/code-server"),
            Ok(Some(PathBuf::from("bin/code-server")))
        );
        assert_eq!(below("top/./a"), Ok(Some(PathBuf::from("a"))));
        assert_eq!(below("./top/a"), Ok(Some(PathBuf::from("a"))));
        // The top folder itself, and nothing at all.
        assert_eq!(below("top/"), Ok(None));
        assert_eq!(below(""), Ok(None));
        assert!(below("top/../../x").is_err());
        assert!(below("/etc/x").is_err());
    }

    #[test]
    fn a_current_file_that_names_no_commit_names_no_build() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(current_build(dir.path()), None);
        std::fs::write(dir.path().join("current"), "../../etc").unwrap();
        assert_eq!(current_build(dir.path()), None);
    }

    #[test]
    fn the_build_follows_the_machines_architecture() {
        assert_eq!(platform("x86_64").unwrap(), "server-linux-x64-web");
        assert_eq!(platform("aarch64").unwrap(), "server-linux-arm64-web");
        let err = platform("riscv64").unwrap_err();
        assert!(err.to_string().contains("riscv64"), "{err}");
    }

    #[test]
    fn the_latest_answer_gives_the_commit_and_the_release() {
        let answer = format!(
            r#"{{"name":"1.143.1","version":"{COMMIT}","productVersion":"1.143.1","timestamp":1}}"#
        );
        assert_eq!(
            parse_latest(&answer).unwrap(),
            Latest {
                commit: COMMIT.to_owned(),
                product_version: "1.143.1".to_owned()
            }
        );
    }

    #[test]
    fn what_goes_into_a_path_or_a_url_is_checked() {
        let with = |commit: &str, release: &str| {
            parse_latest(&format!(
                r#"{{"version":"{commit}","productVersion":"{release}"}}"#
            ))
        };
        assert!(with(COMMIT, "1.144.0-insider").is_ok());
        for (commit, release) in [
            ("../../x", "1.0.0"),
            (&COMMIT[..39], "1.0.0"),
            (&COMMIT.to_uppercase(), "1.0.0"),
            (COMMIT, "1.0/../0"),
            (COMMIT, ""),
            (COMMIT, "1 .0"),
        ] {
            assert!(with(commit, release).is_err(), "{commit} {release}");
        }
        assert!(parse_latest("{}").is_err());
        assert!(parse_latest("nope").is_err());
    }

    #[test]
    fn the_published_hash_must_belong_to_the_build_asked_for() {
        let hash = "4626a9d6f036d382a77197f895eee35cce6843d0a7042ae8be850bec2c77188e";
        let answer = format!(r#"{{"version":"{COMMIT}","sha256hash":"{hash}","hash":"{COMMIT}"}}"#);
        assert_eq!(parse_published_hash(&answer, COMMIT).unwrap(), hash);
        assert!(parse_published_hash(&answer, &"1".repeat(40)).is_err());
        let no_hash = format!(r#"{{"version":"{COMMIT}"}}"#);
        assert!(parse_published_hash(&no_hash, COMMIT).is_err());
    }

    #[test]
    fn the_command_line_carries_the_terms_the_token_file_and_the_telemetry_choice() {
        let server = EditorServer::microsoft("/w/.puddle/vscode-server", "/w", 120, false, true);
        let token = Path::new("/run/puddle/editor-password");
        let args = |telemetry: bool, target: &str| -> Vec<String> {
            arguments(&server, telemetry, token, OsStr::new(target))
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let off = args(false, "/w");
        let after = |flag: &str| {
            let at = off.iter().position(|a| a == flag).unwrap();
            off[at + 1].as_str()
        };
        assert_eq!(after("--host"), "127.0.0.1");
        assert_eq!(after("--port"), "41337");
        assert_eq!(
            after("--connection-token-file"),
            "/run/puddle/editor-password"
        );
        assert_eq!(after("--reconnection-grace-time"), "120");
        assert_eq!(after("--default-folder"), "/w");
        assert!(off.iter().any(|a| a == "--accept-server-license-terms"));
        assert!(off.iter().any(|a| a == "--disable-telemetry"));
        // The data of the server and its extensions sit on the volume, in their own folders.
        let under = |leaf: &str| {
            Path::new("/w/.puddle/vscode-server")
                .join(leaf)
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(after("--server-data-dir"), under("server-data"));
        assert_eq!(after("--extensions-dir"), under("extensions"));
        assert!(!args(true, "/w").iter().any(|a| a == "--disable-telemetry"));
        // The multi-root file is a workspace, not a folder.
        let file = args(false, "/w/.puddle/editor/w.code-workspace");
        assert_eq!(
            file[file.len() - 2..],
            ["--default-workspace", "/w/.puddle/editor/w.code-workspace"]
        );
    }
}
