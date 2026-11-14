// SPDX-License-Identifier: AGPL-3.0-or-later
//! `cargo xtask runtime`: the bundled runtime folder, built in a staging folder and moved into
//! place only when every check passed, so a half-built or unverified folder never exists at `out`.
//!
//! ```text
//! Windows                                   Linux
//! <out>/msb.exe        fork release asset   <out>/msb                   built from the fork tag
//! <out>/libkrunfw.dll  fork release asset    <out>/libkrunfw.so.5        the libkrunfw fork's asset
//!                                                                       (upstream's if none pinned)
//!                                           <out>/libkrunfw.so.<ver>    copy of it, the name msb
//!                                                                       looks for (<ver> is pinned)
//! <out>/runtime.json   version, tags, SHA-256 of msb and the firmware
//! <out>/licenses/      Apache-2.0, LGPL-2.1, GPL-2.0, NOTICE, THIRD-PARTY-{msb,puddle}
//! ```
//!
//! Every download is checked against its release's checksum list and against the committed pin
//! list `ci/msb-runtime.sha256` (a release's own list can be replaced along with its assets), and
//! msb's embedded version against the tag. [`plan`] decides the files; it has no I/O.

use std::path::{Path, PathBuf};

use puddle_runtime::HostOs;
use serde::Serialize;

use crate::checksums::{Checksums, sha256_file};
use crate::error::{Result, XtaskError};
use crate::inventory::Inventory;
use crate::notice;
use crate::pin;
use crate::source::{
    CHECKSUMS_ASSET, FIRMWARE_FORK, Firmware, ForkFacts, LIBKRUNFW_ASSET, LIBKRUNFW_LINUX_ASSET,
    MSB_ASSET, MSB_LINUX_ASSET, Origin, ReleaseSource,
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
    /// Linux: the fork's `msb-linux-x86_64` and the firmware of the libkrunfw fork's pinned
    /// release (upstream's when none is pinned), both downloaded.
    Linux {
        /// An msb to ship instead of the release's, for a tag whose release has no Linux msb
        /// (built by `ci/fetch-msb.sh`); `None` downloads the release's.
        msb: Option<PathBuf>,
        /// The firmware version msb looks for beside itself, from the runtime pin
        /// ([`crate::pin::libkrunfw_linux_version`]).
        libkrunfw_version: String,
        /// The libkrunfw fork release the firmware comes from, from the runtime pin
        /// ([`crate::pin::libkrunfw_linux_tag`]); `None` takes upstream's.
        firmware_tag: Option<String>,
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

/// Which release the firmware of `target` comes from, given the source's firmware facts: the
/// libkrunfw fork's release when the pin names one, upstream's otherwise.
///
/// # Errors
///
/// [`XtaskError::Parse`] when the pin and the facts disagree about the fork release, so the NOTICE
/// would describe another library than the one shipped.
pub fn firmware_origin(target: &Target, firmware: &Firmware) -> Result<Origin> {
    let pinned = match target {
        Target::Windows => None,
        Target::Linux { firmware_tag, .. } => firmware_tag.as_deref(),
    };
    match (pinned, firmware.fork_tag.as_deref()) {
        (None, None) => Ok(Origin::Upstream),
        (Some(pin), Some(facts)) if pin == facts => Ok(Origin::FirmwareFork),
        (pin, facts) => Err(XtaskError::parse(
            "firmware",
            format!(
                "the pin takes the libkrunfw fork release {} but the firmware facts describe {}",
                pin.unwrap_or("none (upstream's)"),
                facts.unwrap_or("none (upstream's)")
            ),
        )),
    }
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
    /// The libkrunfw fork release the firmware comes from; absent for upstream's firmware.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libkrunfw_fork_tag: Option<String>,
}

/// Builds the runtime folder of `target` at `out` for the msb version `expected` (exact embedded
/// version), with `puddle` as the inventory of Puddle's own tree and `pins` the text of the
/// committed pin list ([`crate::pin::PIN_FILE`]) every download is checked against.
///
/// # Errors
///
/// Any failed check (checksum, version, missing licence entry, existing output) or fetch; `out`
/// is left untouched.
pub fn assemble(
    source: &dyn ReleaseSource,
    expected: &str,
    puddle: &Inventory,
    pins: &str,
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
    let result = build(source, expected, puddle, pins, target, &staging);
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
    pin: Pin<'_>,
) -> Result<(PathBuf, String)> {
    mkdir(dir)?;
    let sums_path = source.fetch(origin, CHECKSUMS_ASSET, dir)?;
    let sums_text = std::fs::read_to_string(&sums_path)
        .map_err(|e| XtaskError::io(format!("reading {}", sums_path.display()), e))?;
    let sums = Checksums::parse(list_name, &sums_text)?;
    let file = source.fetch(origin, asset, dir)?;
    let sha = sums.verify(asset, &file)?;
    pin::verify(pin.list, pin.tag, asset, &sha)?;
    Ok((file, sha))
}

/// The committed pin list and the release tag whose lines a download is checked against.
#[derive(Clone, Copy)]
struct Pin<'a> {
    list: &'a str,
    tag: &'a str,
}

/// Downloads and verifies the firmware library. Windows: the fork release's dll, the one
/// `ci/fetch-msb.sh` pins and the VM tests run, so the installer ships the tested firmware. Linux:
/// the pinned libkrunfw fork release, else upstream's.
fn fetch_firmware(
    source: &dyn ReleaseSource,
    facts: &ForkFacts,
    pins: &str,
    os: HostOs,
    target: &Target,
    downloads: &Path,
) -> Result<(PathBuf, String)> {
    let fw_origin = match os {
        HostOs::Windows => Origin::Fork,
        HostOs::Linux | HostOs::MacOs => firmware_origin(target, &facts.firmware)?,
    };
    let fw_list = match (fw_origin, &facts.firmware.fork_tag) {
        (Origin::Fork, _) => format!("{} {} {CHECKSUMS_ASSET}", facts.fork_repo, facts.tag),
        (Origin::FirmwareFork, Some(fork_tag)) => {
            format!("{FIRMWARE_FORK} {fork_tag} {CHECKSUMS_ASSET}")
        }
        _ => format!(
            "{} {} {CHECKSUMS_ASSET}",
            facts.upstream_repo, facts.upstream_tag
        ),
    };
    let pin_tag = match (fw_origin, &facts.firmware.fork_tag) {
        (Origin::Fork, _) => &facts.tag,
        (Origin::FirmwareFork, Some(fork_tag)) => fork_tag,
        _ => &facts.upstream_tag,
    };
    fetch_verified(
        source,
        fw_origin,
        match os {
            HostOs::Windows => LIBKRUNFW_ASSET,
            HostOs::Linux | HostOs::MacOs => LIBKRUNFW_LINUX_ASSET,
        },
        &downloads.join("firmware"),
        &fw_list,
        Pin {
            list: pins,
            tag: pin_tag,
        },
    )
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
    pins: &str,
    target: &Target,
    staging: &Path,
) -> Result<Manifest> {
    let os = target.os();
    let files = os.runtime_files();
    let layout = plan(target)?;
    let facts = source.facts()?;
    let downloads = staging.join(".download");
    let fork_pin = Pin {
        list: pins,
        tag: &facts.tag,
    };
    let (msb, msb_sha256) = match target {
        Target::Windows => fetch_verified(
            source,
            Origin::Fork,
            MSB_ASSET,
            &downloads.join("fork"),
            &format!("{} {} {CHECKSUMS_ASSET}", facts.fork_repo, facts.tag),
            fork_pin,
        )?,
        Target::Linux { msb: Some(msb), .. } => (msb.clone(), sha256_file(msb)?),
        Target::Linux { msb: None, .. } => fetch_verified(
            source,
            Origin::Fork,
            MSB_LINUX_ASSET,
            &downloads.join("fork"),
            &format!("{} {} {CHECKSUMS_ASSET}", facts.fork_repo, facts.tag),
            fork_pin,
        )?,
    };
    let found = puddle_runtime::read_embedded_version(&msb)
        .map_err(|reason| XtaskError::parse(msb.display().to_string(), reason))?;
    if found.as_deref() != Some(expected) {
        return Err(XtaskError::Version {
            expected: expected.to_owned(),
            found: found.unwrap_or_else(|| "none".into()),
        });
    }
    let (fw, libkrunfw_sha256) = fetch_firmware(source, &facts, pins, os, target, &downloads)?;
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
        puddle.render("The Rust crates in Puddle's own programs, with their licences."),
    )?;

    let manifest = Manifest {
        libkrunfw_fork_tag: facts.firmware.fork_tag.clone(),
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
mod tests;
