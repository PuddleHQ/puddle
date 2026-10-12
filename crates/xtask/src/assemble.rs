// SPDX-License-Identifier: GPL-3.0-or-later
//! `cargo xtask runtime`: the bundled runtime folder, built in a staging folder and moved into
//! place only when every check passed, so a half-built or unverified folder never exists at `out`.
//!
//! ```text
//! Windows                                   Linux
//! <out>/msb.exe        fork release asset   <out>/msb                   built from the fork tag
//! <out>/libkrunfw.dll  upstream asset       <out>/libkrunfw.so.5        upstream asset
//!                                           <out>/libkrunfw.so.<ver>    copy of it, the name msb
//!                                                                       looks for (<ver> is pinned)
//! <out>/runtime.json   version, tags, SHA-256 of msb and the firmware
//! <out>/licenses/      Apache-2.0, LGPL-2.1, GPL-2.0, NOTICE, THIRD-PARTY-{msb,puddle}
//! ```
//!
//! Every download is checked against its release's checksum list, and msb's embedded version
//! against the tag. [`plan`] decides the files; it has no I/O.

use std::path::{Path, PathBuf};

use puddle_runtime::HostOs;
use serde::Serialize;

use crate::checksums::{Checksums, sha256_file};
use crate::error::{Result, XtaskError};
use crate::inventory::Inventory;
use crate::notice;
use crate::source::{
    CHECKSUMS_ASSET, LIBKRUNFW_ASSET, LIBKRUNFW_LINUX_ASSET, MSB_ASSET, Origin, ReleaseSource,
};

/// The licence texts shipped with every runtime.
pub const LICENCE_TEXTS: [(&str, &str); 3] = [
    ("Apache-2.0.txt", include_str!("../licenses/Apache-2.0.txt")),
    ("LGPL-2.1.txt", include_str!("../licenses/LGPL-2.1.txt")),
    ("GPL-2.0.txt", include_str!("../licenses/GPL-2.0.txt")),
];

/// Which runtime folder is assembled, with what only that OS needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// Windows: the fork's `msb.exe` and upstream's firmware, both downloaded.
    Windows,
    /// Linux: the fork releases no msb, so `msb` is a binary built from the fork's tag (by
    /// `ci/fetch-msb.sh`); the firmware is downloaded.
    Linux {
        /// The msb binary to ship.
        msb: PathBuf,
        /// The firmware version msb looks for beside itself, from the runtime pin
        /// ([`crate::pin::libkrunfw_linux_version`]).
        libkrunfw_version: String,
    },
}

impl Target {
    /// The OS whose file-name table the folder follows.
    #[must_use]
    pub fn os(&self) -> HostOs {
        match self {
            Self::Windows => HostOs::Windows,
            Self::Linux { .. } => HostOs::Linux,
        }
    }
}

/// Where a file of the runtime folder comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The msb binary.
    Msb,
    /// The firmware library (every copy of it).
    Firmware,
}

/// The files of the runtime folder (without `runtime.json` and `licenses/`), by name, in the
/// order they are placed.
///
/// # Errors
///
/// [`XtaskError::Parse`] when the versioned firmware name would equal the plain one, so the
/// folder would hold one file where msb is promised two.
pub fn plan(target: &Target) -> Result<Vec<(String, Part)>> {
    let files = target.os().runtime_files();
    let mut plan = vec![
        (files.msb.to_owned(), Part::Msb),
        (files.libkrunfw.to_owned(), Part::Firmware),
    ];
    if let Target::Linux {
        libkrunfw_version, ..
    } = target
        && let Some(versioned) = target.os().libkrunfw_versioned(libkrunfw_version)
    {
        if versioned == files.libkrunfw {
            return Err(XtaskError::parse(
                "libkrunfw version",
                format!("{libkrunfw_version} makes the versioned name equal {versioned}"),
            ));
        }
        plan.push((versioned, Part::Firmware));
    }
    Ok(plan)
}

/// What `runtime.json` records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// The embedded msb version (the fork tag without `v`).
    pub version: String,
    /// The fork tag.
    pub fork_tag: String,
    /// Upstream's base tag.
    pub upstream_tag: String,
    /// SHA-256 of msb.
    pub msb_sha256: String,
    /// SHA-256 of the firmware library (every copy of it is this file).
    pub libkrunfw_sha256: String,
    /// The version in the Linux firmware's versioned name; absent on Windows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libkrunfw_version: Option<String>,
}

/// Builds the runtime folder of `target` at `out` for the msb version `expected` (exact embedded
/// version), with `puddle` as the inventory of puddle's own tree.
///
/// # Errors
///
/// Any failed check (checksum, version, missing licence entry, existing output) or fetch; `out`
/// is left untouched.
pub fn assemble(
    source: &dyn ReleaseSource,
    expected: &str,
    puddle: &Inventory,
    target: &Target,
    out: &Path,
) -> Result<Manifest> {
    if out.exists()
        && std::fs::read_dir(out)
            .map_err(|e| XtaskError::io(format!("reading {}", out.display()), e))?
            .next()
            .is_some()
    {
        return Err(XtaskError::OutputExists(out.to_path_buf()));
    }
    let staging = sibling(out, "partial");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|e| XtaskError::io(format!("removing {}", staging.display()), e))?;
    }
    let result = build(source, expected, puddle, target, &staging);
    match result {
        Ok(manifest) => {
            if out.exists() {
                std::fs::remove_dir(out)
                    .map_err(|e| XtaskError::io(format!("removing empty {}", out.display()), e))?;
            }
            std::fs::rename(&staging, out).map_err(|e| {
                XtaskError::io(format!("moving {} into place", staging.display()), e)
            })?;
            Ok(manifest)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging); // best effort; the build error is what matters
            Err(e)
        }
    }
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

fn mkdir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)
        .map_err(|e| XtaskError::io(format!("creating {}", path.display()), e))
}

fn write(path: &Path, content: impl AsRef<[u8]>) -> Result<()> {
    std::fs::write(path, content)
        .map_err(|e| XtaskError::io(format!("writing {}", path.display()), e))
}

fn fetch_verified(
    source: &dyn ReleaseSource,
    origin: Origin,
    asset: &str,
    dir: &Path,
    list_name: &str,
) -> Result<(PathBuf, String)> {
    mkdir(dir)?;
    let sums_path = source.fetch(origin, CHECKSUMS_ASSET, dir)?;
    let sums_text = std::fs::read_to_string(&sums_path)
        .map_err(|e| XtaskError::io(format!("reading {}", sums_path.display()), e))?;
    let sums = Checksums::parse(list_name, &sums_text)?;
    let file = source.fetch(origin, asset, dir)?;
    let sha = sums.verify(asset, &file)?;
    Ok((file, sha))
}

/// Copies the downloaded files into `staging` under the names of `layout`.
fn place(layout: &[(String, Part)], msb: &Path, fw: &Path, staging: &Path) -> Result<()> {
    for (name, part) in layout {
        let from = match part {
            Part::Msb => msb,
            Part::Firmware => fw,
        };
        let to = staging.join(name);
        std::fs::copy(from, &to).map_err(|e| {
            XtaskError::io(format!("copying {} to {}", from.display(), to.display()), e)
        })?;
        // A copy keeps the source's mode, which a downloaded msb lacks.
        #[cfg(unix)]
        if *part == Part::Msb {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| XtaskError::io(format!("making {} executable", to.display()), e))?;
        }
    }
    Ok(())
}

fn build(
    source: &dyn ReleaseSource,
    expected: &str,
    puddle: &Inventory,
    target: &Target,
    staging: &Path,
) -> Result<Manifest> {
    let os = target.os();
    let files = os.runtime_files();
    let layout = plan(target)?;
    let facts = source.facts()?;
    let downloads = staging.join(".download");
    let (msb, msb_sha256) = match target {
        Target::Windows => fetch_verified(
            source,
            Origin::Fork,
            MSB_ASSET,
            &downloads.join("fork"),
            &format!("{} {} {CHECKSUMS_ASSET}", facts.fork_repo, facts.tag),
        )?,
        Target::Linux { msb, .. } => (msb.clone(), sha256_file(msb)?),
    };
    let found = puddle_runtime::read_embedded_version(&msb)
        .map_err(|reason| XtaskError::parse(msb.display().to_string(), reason))?;
    if found.as_deref() != Some(expected) {
        return Err(XtaskError::Version {
            expected: expected.to_owned(),
            found: found.unwrap_or_else(|| "none".into()),
        });
    }
    let (fw, libkrunfw_sha256) = fetch_verified(
        source,
        Origin::Upstream,
        match os {
            HostOs::Windows => LIBKRUNFW_ASSET,
            HostOs::Linux | HostOs::MacOs => LIBKRUNFW_LINUX_ASSET,
        },
        &downloads.join("upstream"),
        &format!(
            "{} {} {CHECKSUMS_ASSET}",
            facts.upstream_repo, facts.upstream_tag
        ),
    )?;
    let msb_inventory = source.msb_inventory(os)?;
    msb_inventory.check()?;
    puddle.check()?;

    place(&layout, &msb, &fw, staging)?;
    std::fs::remove_dir_all(&downloads)
        .map_err(|e| XtaskError::io(format!("removing {}", downloads.display()), e))?;

    let firmware_files = layout
        .iter()
        .filter(|(_, part)| *part == Part::Firmware)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(" and ");
    let licenses = staging.join("licenses");
    mkdir(&licenses)?;
    for (name, text) in LICENCE_TEXTS {
        write(&licenses.join(name), text)?;
    }
    let notice = notice::render(&facts, os, &firmware_files);
    write(&licenses.join("NOTICE.txt"), notice)?;
    write(
        &licenses.join("THIRD-PARTY-msb.txt"),
        msb_inventory.render(&format!(
            "The Rust crates statically linked into {} ({} at {}), with their licences.",
            files.msb, facts.fork_repo, facts.tag
        )),
    )?;
    write(
        &licenses.join("THIRD-PARTY-puddle.txt"),
        puddle.render("The Rust crates in puddle's own programs, with their licences."),
    )?;

    let manifest = Manifest {
        version: expected.to_owned(),
        fork_tag: facts.tag,
        upstream_tag: facts.upstream_tag,
        msb_sha256,
        libkrunfw_sha256,
        libkrunfw_version: match target {
            Target::Windows => None,
            Target::Linux {
                libkrunfw_version, ..
            } => Some(libkrunfw_version.clone()),
        },
    };
    let json = serde_json::to_string_pretty(&manifest)
        .map_err(|e| XtaskError::parse("runtime.json", e))?;
    write(&staging.join("runtime.json"), json + "\n")?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use puddle_runtime::RuntimeFiles;

    #[test]
    fn licence_texts_are_the_real_ones() {
        let [(_, apache), (_, lgpl), (_, gpl)] = LICENCE_TEXTS;
        assert!(apache.contains("Apache License") && apache.contains("Version 2.0, January 2004"));
        assert!(
            lgpl.contains("GNU LESSER GENERAL PUBLIC LICENSE")
                && lgpl.contains("Version 2.1, February 1999")
        );
        assert!(gpl.contains("GNU GENERAL PUBLIC LICENSE") && gpl.contains("Version 2, June 1991"));
    }

    const WIN: RuntimeFiles = HostOs::Windows.runtime_files();
    const LINUX: RuntimeFiles = HostOs::Linux.runtime_files();

    fn linux(version: &str) -> Target {
        Target::Linux {
            msb: PathBuf::from("msb"),
            libkrunfw_version: version.to_owned(),
        }
    }

    fn fw(name: &str) -> (String, Part) {
        (name.to_owned(), Part::Firmware)
    }

    #[test]
    fn windows_ships_msb_and_one_firmware_file() {
        assert_eq!(
            plan(&Target::Windows).unwrap(),
            [(WIN.msb.to_owned(), Part::Msb), fw(WIN.libkrunfw)]
        );
    }

    #[test]
    fn linux_ships_the_firmware_under_its_soname_and_the_pinned_version() {
        assert_eq!(
            plan(&linux("5.6.1")).unwrap(),
            [
                (LINUX.msb.to_owned(), Part::Msb),
                fw(LINUX.libkrunfw),
                fw(&format!("{}.6.1", LINUX.libkrunfw))
            ]
        );
        // A pin bump changes the name.
        assert_eq!(
            plan(&linux("5.7.0")).unwrap().last(),
            Some(&fw(&format!("{}.7.0", LINUX.libkrunfw)))
        );
    }

    #[test]
    fn a_version_that_equals_the_soname_is_refused() {
        let err = plan(&linux("5")).unwrap_err().to_string();
        assert!(err.contains(LINUX.libkrunfw), "{err}");
    }

    #[test]
    fn a_part_that_cannot_be_copied_is_named() {
        let dir = std::env::temp_dir();
        let missing = dir.join("xtask-assemble-no-such-msb");
        let err = place(&[("msb".to_owned(), Part::Msb)], &missing, &missing, &dir)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("copying") && err.contains("no-such-msb"),
            "{err}"
        );
    }

    #[test]
    fn staging_sits_beside_the_output() {
        assert_eq!(
            sibling(Path::new("/a/runtime"), "partial"),
            Path::new("/a/runtime.partial")
        );
    }
}
