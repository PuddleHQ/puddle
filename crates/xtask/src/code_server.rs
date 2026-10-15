// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask code-server`: the bundled browser editor server, a pinned code-server release
//! unpacked into a folder the host mounts read-only into every guest.
//!
//! The release is the upstream `linux-amd64` tarball (it carries its own `node`). It is fetched
//! from GitHub, checked against the SHA-256 in `ci/code-server.sha256` and unpacked into a
//! staging folder that is moved to the output only when every check passed:
//!
//! ```text
//! <out>/bin/code-server, lib/node, out/, node_modules/ ...   the release, as published
//! <out>/LICENSE, ThirdPartyNotices.txt                       its licence (MIT) and notices
//! <out>/PUDDLE-NOTICE.txt                                    what it is and where it came from
//! <out>/code-server.json                                     version, asset, SHA-256, counts
//! ```
//!
//! The tarball's only symbolic links are the command shims in `node_modules/.bin`, which nothing
//! at run time uses; they are left out so the folder unpacks the same on Windows, where a link
//! needs a privilege. Any other link, a path that leaves the folder, or two names that differ only
//! in case (they would overwrite each other on Windows) fails the build.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;
use serde::Serialize;
use tar::EntryType;

use crate::checksums::sha256_file;
use crate::error::{Result, XtaskError};

/// The pin list, relative to the workspace root.
pub const PIN_FILE: &str = "ci/code-server.sha256";

/// The repository the releases come from.
pub const REPOSITORY: &str = "coder/code-server";

/// The links a release is allowed to hold, and the only ones skipped: command shims.
const SHIM_DIR: &str = "node_modules/.bin/";

/// Files the unpacked folder must hold for the agent to start it.
const REQUIRED: [&str; 4] = ["bin/code-server", "lib/node", "package.json", "LICENSE"];

/// The notice files copied to a licences folder.
const NOTICE_FILES: [&str; 2] = ["LICENSE", "ThirdPartyNotices.txt"];

/// One line of the pin list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// The code-server version, `4.141.0`.
    pub version: String,
    /// The release asset's name.
    pub asset: String,
    /// The asset's SHA-256, lowercase hex.
    pub sha256: String,
}

/// Parses the pin list: `<version> <asset> <sha256>` per line, `#` lines and blanks ignored.
///
/// # Errors
///
/// [`XtaskError::Parse`] for a malformed line, a version listed twice, or an empty list.
pub fn parse_pins(text: &str) -> Result<Vec<Pin>> {
    let mut pins: Vec<Pin> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = |why: &str| XtaskError::parse(PIN_FILE, format!("line {}: {why}", n + 1));
        let mut fields = line.split_whitespace();
        let (Some(version), Some(asset), Some(sha), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            return Err(bad("expected `<version> <asset> <sha256>`"));
        };
        let plain = version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        if !plain {
            return Err(bad("the version is not a dotted number"));
        }
        if asset.contains(['/', '\\']) {
            return Err(bad("the asset is a file name, not a path"));
        }
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("not a SHA-256"));
        }
        if pins.iter().any(|p| p.version == version) {
            return Err(bad("version listed twice"));
        }
        pins.push(Pin {
            version: version.to_owned(),
            asset: asset.to_owned(),
            sha256: sha.to_ascii_lowercase(),
        });
    }
    if pins.is_empty() {
        return Err(XtaskError::parse(PIN_FILE, "no version is pinned"));
    }
    Ok(pins)
}

/// The pin for `version`, or the first one when none is asked for.
///
/// # Errors
///
/// [`XtaskError::Parse`] when `version` is not in the list.
pub fn select<'a>(pins: &'a [Pin], version: Option<&str>) -> Result<&'a Pin> {
    let found = match version {
        Some(v) => pins.iter().find(|p| p.version == v),
        None => pins.first(),
    };
    found.ok_or_else(|| {
        XtaskError::parse(
            PIN_FILE,
            format!(
                "code-server {} is not pinned (pinned: {})",
                version.unwrap_or("(none)"),
                pins.iter()
                    .map(|p| p.version.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    })
}

/// Where the release tarball comes from.
pub trait CodeServerSource {
    /// Puts release asset `asset` of code-server `version` into `dir` and returns its path.
    ///
    /// # Errors
    ///
    /// When the asset can't be fetched.
    fn fetch(&self, version: &str, asset: &str, dir: &Path) -> Result<PathBuf>;
}

/// `gh release download` from [`REPOSITORY`].
#[derive(Debug, Clone, Copy)]
pub struct GitHubReleases;

impl CodeServerSource for GitHubReleases {
    fn fetch(&self, version: &str, asset: &str, dir: &Path) -> Result<PathBuf> {
        crate::tools::run(
            std::process::Command::new("gh")
                .args([
                    "release",
                    "download",
                    &format!("v{version}"),
                    "--repo",
                    REPOSITORY,
                    "--pattern",
                    asset,
                    "--clobber",
                    "--dir",
                ])
                .arg(dir),
        )?;
        Ok(dir.join(asset))
    }
}

/// A folder holding the asset, for tests and offline runs (`--from`).
#[derive(Debug, Clone)]
pub struct DirReleases(pub PathBuf);

impl CodeServerSource for DirReleases {
    fn fetch(&self, _version: &str, asset: &str, dir: &Path) -> Result<PathBuf> {
        let from = self.0.join(asset);
        let to = dir.join(asset);
        std::fs::copy(&from, &to)
            .map_err(|e| XtaskError::io(format!("copying {}", from.display()), e))?;
        Ok(to)
    }
}

/// What `code-server.json` records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// The code-server version.
    pub version: String,
    /// The release asset.
    pub asset: String,
    /// The asset's SHA-256.
    pub sha256: String,
    /// Regular files unpacked.
    pub files: usize,
    /// Command shims left out (see the module docs).
    pub skipped_links: usize,
}

/// What unpacking found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unpacked {
    /// Regular files written.
    pub files: usize,
    /// Command shims skipped.
    pub skipped_links: usize,
}

fn mkdir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|e| XtaskError::io(format!("creating {}", path.display()), e))
}

/// The entry's path below the release's top folder, `/`-separated; `None` for the top folder
/// itself.
fn relative(path: &Path) -> Result<Option<String>> {
    let bad = |why: &str| XtaskError::parse("the code-server tarball", format!("{why}: {path:?}"));
    let mut parts = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(p) => parts.push(
                p.to_str()
                    .ok_or_else(|| bad("a name that is not UTF-8"))?
                    .to_owned(),
            ),
            Component::CurDir => {}
            _ => return Err(bad("a path that leaves the folder")),
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    parts.remove(0); // `code-server-<version>-linux-amd64/`
    Ok((!parts.is_empty()).then(|| parts.join("/")))
}

/// Unpacks the release tarball at `tarball` into `dest` (created), without its top folder.
///
/// # Errors
///
/// [`XtaskError::Io`] for the file system; [`XtaskError::Parse`] for a path that leaves `dest`,
/// two names that differ only in case, or a link other than a command shim.
pub fn unpack(tarball: &Path, dest: &Path) -> Result<Unpacked> {
    let file = std::fs::File::open(tarball)
        .map_err(|e| XtaskError::io(format!("opening {}", tarball.display()), e))?;
    let what = || format!("reading {}", tarball.display());
    let mut archive = tar::Archive::new(GzDecoder::new(file));
    mkdir(dest)?;
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut out = Unpacked {
        files: 0,
        skipped_links: 0,
    };
    for entry in archive.entries().map_err(|e| XtaskError::io(what(), e))? {
        let mut entry = entry.map_err(|e| XtaskError::io(what(), e))?;
        let path = entry.path().map_err(|e| XtaskError::io(what(), e))?;
        let Some(name) = relative(&path)? else {
            continue;
        };
        if let Some(other) = seen.insert(name.to_lowercase(), name.clone())
            && other != name
        {
            return Err(XtaskError::parse(
                "the code-server tarball",
                format!("{name} and {other} differ only in case"),
            ));
        }
        let target = dest.join(&name);
        match entry.header().entry_type() {
            EntryType::Directory => mkdir(&target)?,
            EntryType::Regular | EntryType::Continuous => {
                if let Some(parent) = target.parent() {
                    mkdir(parent)?;
                }
                let mut file = std::fs::File::create(&target)
                    .map_err(|e| XtaskError::io(format!("creating {}", target.display()), e))?;
                std::io::copy(&mut entry, &mut file)
                    .map_err(|e| XtaskError::io(format!("writing {}", target.display()), e))?;
                #[cfg(unix)]
                if let Ok(mode) = entry.header().mode() {
                    use std::os::unix::fs::PermissionsExt;
                    // Owner read/write always (a read-only file would block the next unpack);
                    // the execute bits as published.
                    let mode = (mode & 0o755) | 0o644;
                    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode))
                        .map_err(|e| {
                            XtaskError::io(format!("setting the mode of {}", target.display()), e)
                        })?;
                }
                out.files += 1;
            }
            EntryType::Symlink | EntryType::Link if name.starts_with(SHIM_DIR) => {
                out.skipped_links += 1;
            }
            other => {
                return Err(XtaskError::parse(
                    "the code-server tarball",
                    format!("{name} is a {other:?} entry, which the bundle does not carry"),
                ));
            }
        }
    }
    Ok(out)
}

/// The version in the unpacked folder's `package.json`.
fn package_version(dir: &Path) -> Result<String> {
    let path = dir.join("package.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| XtaskError::io(format!("reading {}", path.display()), e))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| XtaskError::parse("package.json", e))?;
    json.get("version")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| XtaskError::parse("package.json", "no version"))
}

/// What `PUDDLE-NOTICE.txt` says.
fn notice(pin: &Pin) -> String {
    format!(
        "code-server {version}\n\
         \n\
         This folder is the unmodified code-server {version} release asset {asset}\n\
         (SHA-256 {sha}) from https://github.com/{REPOSITORY}/releases/tag/v{version},\n\
         copyright Coder Technologies, MIT licence (LICENSE). The third-party components it\n\
         contains, node included, are listed with their licences in ThirdPartyNotices.txt.\n\
         Puddle mounts it read-only into each workspace and starts it there; it changes it\n\
         only by leaving out the command shims of node_modules/.bin.\n",
        version = pin.version,
        asset = pin.asset,
        sha = pin.sha256,
    )
}

/// Builds the bundle for `pin` at `out`, which must not exist or be empty. With `licences`, the
/// licence and the notices are also copied to `<licences>/code-server/`.
///
/// # Errors
///
/// Any failed check (checksum, version, missing file, a path or link the tarball must not hold)
/// or fetch; `out` is left untouched.
pub fn assemble(
    source: &dyn CodeServerSource,
    pin: &Pin,
    out: &Path,
    licences: Option<&Path>,
) -> Result<Manifest> {
    if out.exists()
        && std::fs::read_dir(out)
            .map_err(|e| XtaskError::io(format!("reading {}", out.display()), e))?
            .next()
            .is_some()
    {
        return Err(XtaskError::OutputExists(out.to_path_buf()));
    }
    let mut name = out.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    let staging = out.with_file_name(name);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| XtaskError::io(format!("removing {}", staging.display()), e))?;
    }
    let built = build(source, pin, &staging);
    match built {
        Ok(manifest) => {
            if out.exists() {
                std::fs::remove_dir(out)
                    .map_err(|e| XtaskError::io(format!("removing empty {}", out.display()), e))?;
            }
            std::fs::rename(&staging, out).map_err(|e| {
                XtaskError::io(format!("moving {} into place", staging.display()), e)
            })?;
            if let Some(dir) = licences {
                copy_notices(out, &dir.join("code-server"))?;
            }
            Ok(manifest)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging); // best effort; the build error is what matters
            Err(e)
        }
    }
}

fn copy_notices(bundle: &Path, to: &Path) -> Result<()> {
    mkdir(to)?;
    for file in NOTICE_FILES {
        let (from, dest) = (bundle.join(file), to.join(file));
        std::fs::copy(&from, &dest)
            .map_err(|e| XtaskError::io(format!("copying {}", from.display()), e))?;
    }
    Ok(())
}

fn build(source: &dyn CodeServerSource, pin: &Pin, staging: &Path) -> Result<Manifest> {
    let downloads = staging.join(".download");
    mkdir(&downloads)?;
    let tarball = source.fetch(&pin.version, &pin.asset, &downloads)?;
    let actual = sha256_file(&tarball)?;
    if actual != pin.sha256 {
        return Err(XtaskError::Checksum {
            file: pin.asset.clone(),
            list: PIN_FILE.to_owned(),
            expected: pin.sha256.clone(),
            actual,
        });
    }
    let unpacked = unpack(&tarball, staging)?;
    std::fs::remove_dir_all(&downloads)
        .map_err(|e| XtaskError::io(format!("removing {}", downloads.display()), e))?;
    for required in REQUIRED.into_iter().chain(NOTICE_FILES) {
        if !staging.join(required).is_file() {
            return Err(XtaskError::parse(
                pin.asset.as_str(),
                format!("the release has no {required}"),
            ));
        }
    }
    let found = package_version(staging)?;
    if found != pin.version {
        return Err(XtaskError::Version {
            expected: pin.version.clone(),
            found,
        });
    }
    let manifest = Manifest {
        version: pin.version.clone(),
        asset: pin.asset.clone(),
        sha256: pin.sha256.clone(),
        files: unpacked.files,
        skipped_links: unpacked.skipped_links,
    };
    let write = |name: &str, content: String| {
        let path = staging.join(name);
        std::fs::write(&path, content)
            .map_err(|e| XtaskError::io(format!("writing {}", path.display()), e))
    };
    write("PUDDLE-NOTICE.txt", notice(pin))?;
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| XtaskError::parse("code-server.json", e))?;
    write("code-server.json", json + "\n")?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Read;

    fn head(path: &Path, limit: u64) -> String {
        let mut text = String::new();
        let f = std::fs::File::open(path).unwrap();
        f.take(limit).read_to_string(&mut text).unwrap();
        text
    }

    const TOP: &str = "code-server-9.9.9-linux-amd64";

    /// A tarball with `files` (path below the top folder, content, mode) and `links`.
    fn tarball(dir: &Path, files: &[(&str, &str, u32)], links: &[&str]) -> PathBuf {
        let path = dir.join("code-server-9.9.9-linux-amd64.tar.gz");
        let gz = GzEncoder::new(std::fs::File::create(&path).unwrap(), Compression::fast());
        let mut b = tar::Builder::new(gz);
        for (name, content, mode) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(content.len() as u64);
            h.set_mode(*mode);
            h.set_cksum();
            b.append_data(&mut h, format!("{TOP}/{name}"), content.as_bytes())
                .unwrap();
        }
        for link in links {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(EntryType::Symlink);
            h.set_size(0);
            h.set_cksum();
            b.append_link(&mut h, format!("{TOP}/{link}"), "../x/y.js")
                .unwrap();
        }
        b.into_inner().unwrap().finish().unwrap();
        path
    }

    fn release(version: &str) -> Vec<(&'static str, String, u32)> {
        vec![
            ("bin/code-server", "#!/bin/sh\n".into(), 0o755),
            ("lib/node", "node".into(), 0o755),
            (
                "package.json",
                format!("{{\"version\":\"{version}\"}}"),
                0o644,
            ),
            ("LICENSE", "MIT".into(), 0o644),
            ("ThirdPartyNotices.txt", "notices".into(), 0o644),
        ]
    }

    fn make(dir: &Path, version: &str, links: &[&str]) -> (DirReleases, Pin) {
        let files = release(version);
        let refs: Vec<(&str, &str, u32)> =
            files.iter().map(|(n, c, m)| (*n, c.as_str(), *m)).collect();
        let path = tarball(dir, &refs, links);
        let sha = sha256_file(&path).unwrap();
        (
            DirReleases(dir.to_path_buf()),
            Pin {
                version: "9.9.9".into(),
                asset: path.file_name().unwrap().to_string_lossy().into_owned(),
                sha256: sha,
            },
        )
    }

    #[test]
    fn the_pin_list_in_the_repository_parses() {
        let text = std::fs::read_to_string(crate::workspace_root().join(PIN_FILE)).unwrap();
        let pins = parse_pins(&text).unwrap();
        let first = select(&pins, None).unwrap();
        assert!(
            first.asset.contains(&first.version) && first.asset.ends_with("linux-amd64.tar.gz")
        );
        assert_eq!(select(&pins, Some(&first.version)).unwrap(), first);
    }

    #[test]
    fn pin_lines_are_checked() {
        let sha = "a".repeat(64);
        for (text, needle) in [
            ("", "no version is pinned"),
            ("# only a comment\n", "no version is pinned"),
            ("4.1.0 asset", "expected"),
            ("4.1.0 asset sum", "not a SHA-256"),
            (&format!("4.x asset {sha}"), "not a dotted number"),
            (&format!("4.1.0 a/b {sha}"), "not a path"),
            (&format!("4.1.0 a {sha} extra"), "expected"),
            (&format!("4.1.0 a {sha}\n4.1.0 b {sha}"), "listed twice"),
        ] {
            let err = parse_pins(text).unwrap_err().to_string();
            assert!(err.contains(needle), "{text:?}: {err}");
        }
        let pins = parse_pins(&format!(
            "# c\n4.2.0 a {sha}\n4.1.0 b {}\n",
            sha.to_uppercase()
        ))
        .unwrap();
        assert_eq!(select(&pins, None).unwrap().version, "4.2.0");
        assert_eq!(select(&pins, Some("4.1.0")).unwrap().sha256, sha);
        let err = select(&pins, Some("3.0.0")).unwrap_err().to_string();
        assert!(
            err.contains("not pinned") && err.contains("4.2.0, 4.1.0"),
            "{err}"
        );
    }

    #[test]
    fn builds_the_folder_with_its_notices_and_leaves_command_shims_out() {
        let dir = tempfile::tempdir().unwrap();
        let (source, pin) = make(dir.path(), "9.9.9", &["node_modules/.bin/semver"]);
        let out = dir.path().join("bundle");
        let licences = dir.path().join("licenses");
        let manifest = assemble(&source, &pin, &out, Some(&licences)).unwrap();
        assert_eq!(manifest.files, 5);
        assert_eq!(manifest.skipped_links, 1);
        assert!(out.join("lib/node").is_file() && out.join("bin/code-server").is_file());
        assert!(!out.join("node_modules/.bin/semver").exists());
        assert!(!out.join(".download").exists());
        assert!(!dir.path().join("bundle.partial").exists());
        let note = std::fs::read_to_string(out.join("PUDDLE-NOTICE.txt")).unwrap();
        assert!(
            note.contains(&pin.sha256) && note.contains("MIT licence"),
            "{note}"
        );
        let json = std::fs::read_to_string(out.join("code-server.json")).unwrap();
        assert!(json.contains("\"version\": \"9.9.9\"") && json.contains(&pin.sha256));
        assert_eq!(head(&licences.join("code-server/LICENSE"), 10), "MIT");
        assert_eq!(
            head(&licences.join("code-server/ThirdPartyNotices.txt"), 20),
            "notices"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |p: &str| std::fs::metadata(out.join(p)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode("lib/node"), 0o755);
            assert_eq!(mode("LICENSE"), 0o644);
        }
    }

    #[test]
    fn a_changed_download_is_refused_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (source, mut pin) = make(dir.path(), "9.9.9", &[]);
        pin.sha256 = "0".repeat(64);
        let out = dir.path().join("bundle");
        let err = assemble(&source, &pin, &out, None).unwrap_err().to_string();
        assert!(
            err.contains("checksum mismatch") && err.contains(PIN_FILE),
            "{err}"
        );
        assert!(!out.exists() && !dir.path().join("bundle.partial").exists());
    }

    #[test]
    fn a_release_of_another_version_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (source, pin) = make(dir.path(), "9.9.8", &[]);
        let err = assemble(&source, &pin, &dir.path().join("b"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("9.9.8") && err.contains("9.9.9"), "{err}");
    }

    #[test]
    fn a_missing_file_a_full_output_and_a_stray_link_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = tarball(
            dir.path(),
            &[("package.json", "{\"version\":\"9.9.9\"}", 0o644)],
            &[],
        );
        let pin = Pin {
            version: "9.9.9".into(),
            asset: path.file_name().unwrap().to_string_lossy().into_owned(),
            sha256: sha256_file(&path).unwrap(),
        };
        let source = DirReleases(dir.path().to_path_buf());
        let err = assemble(&source, &pin, &dir.path().join("b"), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("has no bin/code-server"), "{err}");

        let full = dir.path().join("full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("x"), "x").unwrap();
        assert!(matches!(
            assemble(&source, &pin, &full, None),
            Err(XtaskError::OutputExists(_))
        ));

        let stray = tarball(dir.path(), &[], &["lib/evil-link"]);
        let err = unpack(&stray, &dir.path().join("u"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("lib/evil-link") && err.contains("Symlink"),
            "{err}"
        );
    }

    #[test]
    fn names_that_differ_only_in_case_fail_the_unpack() {
        let dir = tempfile::tempdir().unwrap();
        let path = tarball(
            dir.path(),
            &[("a/Readme", "1", 0o644), ("a/README", "2", 0o644)],
            &[],
        );
        let err = unpack(&path, &dir.path().join("u"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("differ only in case"), "{err}");
        // The same name twice (a later entry replaces an earlier one) is not a collision.
        let path = tarball(dir.path(), &[("a/x", "1", 0o644), ("a/x", "2", 0o644)], &[]);
        assert_eq!(unpack(&path, &dir.path().join("v")).unwrap().files, 2);
    }

    #[test]
    fn a_path_that_leaves_the_folder_is_refused() {
        assert!(relative(Path::new("top/../../x")).is_err());
        assert!(relative(Path::new("/etc/passwd")).is_err());
        assert_eq!(relative(Path::new("top")).unwrap(), None);
        assert_eq!(
            relative(Path::new("top/a/b")).unwrap().as_deref(),
            Some("a/b")
        );
    }
}
