// SPDX-License-Identifier: GPL-3.0-or-later
//! The pinned mise in the guest: downloaded, checked against the SHA-256 in `ci/mise.sha256`,
//! unpacked on the workspace volume. The host starts it as `puddle-agent mise-fetch <dir>` after a
//! start has settled; this module never decides whether mise is wanted.
//!
//! Unlike the VS Code server ([`crate::microsoft`]), whose hash comes from the service that serves
//! the file, the hash here is compiled into the agent, so a match also proves the file is the one
//! this Puddle was built against.
//!
//! What lands on the volume:
//!
//! ```text
//! <dir>/bin/mise          the binary (mode 0755)
//! <dir>/bin/.puddle-mise  marker: "<version> <tarball sha256> <binary sha256>", written last
//! ```
//!
//! A marker that names the pinned version and tarball, with a binary whose hash is still the one
//! in the marker, means there is nothing to do. A binary that changed since (a `mise self-update`,
//! a damaged file) is downloaded again. Downloading goes through [`Fetch`], so the rest runs
//! without a network in tests.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::EntryType;

use crate::download::{Fetch, sha256_of};

/// The pins, one line per guest architecture: `<version> <asset> <sha256>`.
pub const PINS: &str = include_str!("../../../ci/mise.sha256");

/// Where the releases are downloaded from (`/v<version>/<asset>`, served directly).
const SITE: &str = "https://mise.jdx.dev";

/// The binary's path inside the tarball.
const IN_ARCHIVE: &str = "mise/bin/mise";

/// The binary's path below the folder the host names.
const BINARY: &str = "bin/mise";

/// The marker's path below the folder the host names.
const MARKER: &str = "bin/.puddle-mise";

/// The most a binary may unpack to (the real one is about 150 MB): a tarball that expands further
/// is refused instead of filling the volume.
const MAX_BINARY: u64 = 512 * 1024 * 1024;

/// What the host sends: the folder (`<volume>/.puddle/mise`) the binary goes below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The folder that holds `bin/mise`.
    pub dir: PathBuf,
}

/// Usage text of the subcommand.
pub const USAGE: &str = "usage: puddle-agent mise-fetch <dir>";

impl Request {
    /// Reads `<dir>`.
    ///
    /// # Errors
    ///
    /// The usage text when the arguments are not exactly one non-empty folder.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Self, String> {
        match args {
            [dir] if !dir.as_ref().is_empty() => Ok(Self {
                dir: PathBuf::from(dir.as_ref()),
            }),
            _ => Err(USAGE.to_owned()),
        }
    }
}

/// One pinned release for one architecture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The release, without the `v`.
    pub version: String,
    /// The tarball's file name.
    pub asset: String,
    /// Its SHA-256, lower-case hex.
    pub sha256: String,
}

impl Pin {
    /// The pinned release for the machine this agent runs on.
    ///
    /// # Errors
    ///
    /// [`MiseError::Unsupported`] when the file pins none for this architecture.
    pub fn current() -> Result<Self, MiseError> {
        Self::for_arch(PINS, std::env::consts::ARCH)
    }

    /// The release the first line of `pins` names, for Rust architecture name `arch`
    /// (`x86_64`, `aarch64`).
    ///
    /// # Errors
    ///
    /// [`MiseError::Unsupported`] for an architecture with no line, [`MiseError::Pins`] for a
    /// line that is not `<version> <asset> <64 hex digits>`.
    pub fn for_arch(pins: &str, arch: &str) -> Result<Self, MiseError> {
        let name = match arch {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            other => return Err(MiseError::Unsupported(other.to_owned())),
        };
        let mut lines = pins
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| l.split_whitespace().collect::<Vec<_>>());
        let version = lines
            .clone()
            .next()
            .and_then(|f| f.first().copied())
            .ok_or_else(|| MiseError::Pins("no pinned release".to_owned()))?
            .to_owned();
        let asset = format!("mise-v{version}-linux-{name}-musl.tar.gz");
        let Some(line) = lines
            .find(|f| f.first() == Some(&version.as_str()) && f.get(1) == Some(&asset.as_str()))
        else {
            return Err(MiseError::Unsupported(arch.to_owned()));
        };
        let sha256 = match line.as_slice() {
            [_, _, hash]
                if hash.len() == 64
                    && hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                (*hash).to_owned()
            }
            _ => {
                return Err(MiseError::Pins(format!(
                    "the line for {asset} is malformed"
                )));
            }
        };
        Ok(Self {
            version,
            asset,
            sha256,
        })
    }

    /// Where the tarball is downloaded.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{SITE}/v{}/{}", self.version, self.asset)
    }

    /// What the marker holds once the binary hash `binary` is known.
    fn marker(&self, binary: &str) -> String {
        format!("{} {} {binary}\n", self.version, self.sha256)
    }
}

/// Why mise could not be put on the volume. The text is what the user is shown.
#[derive(Debug, thiserror::Error)]
pub enum MiseError {
    /// No build is pinned for this machine's architecture.
    #[error("mise has no build for the {0} architecture pinned in this Puddle")]
    Unsupported(String),
    /// The pin file is damaged (a build-time bug, caught by a test).
    #[error("the mise pin file is unusable: {0}")]
    Pins(String),
    /// The image has neither `curl` nor `wget`.
    #[error("the image has neither curl nor wget, which Puddle needs to download mise")]
    NoDownloader,
    /// The tarball could not be downloaded.
    #[error("Puddle could not download mise: {0}")]
    Download(String),
    /// The download is not the pinned file.
    #[error("the mise download does not match the checksum Puddle expects")]
    Checksum {
        /// The pinned hash.
        expected: String,
        /// The hash of what arrived.
        actual: String,
    },
    /// The tarball does not hold the binary where it should.
    #[error("the mise download is not unpackable: {0}")]
    Archive(String),
    /// The volume could not be written.
    #[error("Puddle could not write mise to the workspace volume: {0}")]
    Io(#[from] io::Error),
}

impl MiseError {
    /// Whether the download arrived and was refused: the same file is refused again, so a retry
    /// before the next start is a waste of 50 MB. A network that is down is not one.
    #[must_use]
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Checksum { .. } | Self::Archive(_))
    }
}

/// What [`fetch`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The pinned binary is on the volume, as it was found.
    Current,
    /// The pinned binary was downloaded and put on the volume.
    Installed,
}

impl Outcome {
    /// The word the subcommand prints.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Installed => "installed",
        }
    }
}

/// Whether `dir` holds exactly the pinned binary: the marker names the pin and the file still has
/// the hash the marker recorded.
fn is_current(dir: &Path, pin: &Pin) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(MARKER)) else {
        return false;
    };
    let [version, tarball, binary] = text.split_whitespace().collect::<Vec<_>>()[..] else {
        return false;
    };
    version == pin.version
        && tarball == pin.sha256
        && sha256_of(&dir.join(BINARY)).is_ok_and(|actual| actual == binary)
}

/// Puts the pinned binary below `dir` unless it is already there.
///
/// # Errors
///
/// [`MiseError`], naming what failed. Nothing half-written stays: the marker is written last, so
/// a folder without a matching marker is downloaded again.
pub async fn fetch(fetch: &impl Fetch, pin: &Pin, dir: &Path) -> Result<Outcome, MiseError> {
    let (check_dir, check_pin) = (dir.to_owned(), pin.clone());
    if tokio::task::spawn_blocking(move || is_current(&check_dir, &check_pin))
        .await
        .map_err(|e| io::Error::other(format!("the check task stopped: {e}")))?
    {
        return Ok(Outcome::Current);
    }
    tokio::fs::create_dir_all(dir.join("bin")).await?;
    let tarball = dir.join(".mise-download.tar.gz");
    let result = download_and_unpack(fetch, pin, dir, &tarball).await;
    // The 50 MB tarball is never kept, whatever happened.
    let _ = tokio::fs::remove_file(&tarball).await;
    result.map(|()| Outcome::Installed)
}

async fn download_and_unpack(
    fetch: &impl Fetch,
    pin: &Pin,
    dir: &Path,
    tarball: &Path,
) -> Result<(), MiseError> {
    // A marker of an older install must not survive a replaced binary.
    match tokio::fs::remove_file(dir.join(MARKER)).await {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    fetch.file(&pin.url(), tarball).await.map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            MiseError::NoDownloader
        } else {
            MiseError::Download(e.to_string())
        }
    })?;
    let (tar, dest, pin) = (tarball.to_owned(), dir.to_owned(), pin.clone());
    tokio::task::spawn_blocking(move || {
        let actual = sha256_of(&tar)?;
        if actual != pin.sha256 {
            return Err(MiseError::Checksum {
                expected: pin.sha256,
                actual,
            });
        }
        let partial = dest.join("bin/.mise.partial");
        let unpacked = unpack_binary(&tar, &partial);
        let installed = unpacked.and_then(|()| {
            let binary = sha256_of(&partial)?;
            std::fs::rename(&partial, dest.join(BINARY))?;
            Ok(binary)
        });
        let binary = match installed {
            Ok(binary) => binary,
            Err(err) => {
                let _ = std::fs::remove_file(&partial);
                return Err(err);
            }
        };
        let marker_partial = dest.join("bin/.puddle-mise.partial");
        std::fs::write(&marker_partial, pin.marker(&binary))?;
        std::fs::rename(&marker_partial, dest.join(MARKER))?;
        Ok(())
    })
    .await
    .map_err(|e| io::Error::other(format!("the unpack task stopped: {e}")))?
}

/// Writes the binary of the tarball at `tarball` to `out` (mode 0755). The rest of the tarball
/// (manual page, shell snippets, licence) is not unpacked.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "tar's EntryType has a hidden catch-all variant, so the kinds cannot be listed"
)]
fn unpack_binary(tarball: &Path, out: &Path) -> Result<(), MiseError> {
    let archive_error = |e: io::Error| MiseError::Archive(e.to_string());
    let mut archive = tar::Archive::new(GzDecoder::new(std::fs::File::open(tarball)?));
    for entry in archive.entries().map_err(archive_error)? {
        let entry = entry.map_err(archive_error)?;
        let path = entry.path().map_err(archive_error)?.into_owned();
        if path.as_path() != Path::new(IN_ARCHIVE) {
            continue;
        }
        match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => {}
            other => {
                return Err(MiseError::Archive(format!(
                    "{IN_ARCHIVE} as a {other:?} entry"
                )));
            }
        }
        let mut file = std::fs::File::create(out)?;
        // One byte past the limit tells a long file from one that fits exactly.
        let copied = io::copy(&mut entry.take(MAX_BINARY + 1), &mut file)?;
        if copied > MAX_BINARY {
            return Err(MiseError::Archive(format!(
                "{IN_ARCHIVE} larger than {MAX_BINARY} bytes"
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755))?;
        }
        return Ok(());
    }
    Err(MiseError::Archive(format!("no {IN_ARCHIVE}")))
}

#[cfg(test)]
mod tests;
