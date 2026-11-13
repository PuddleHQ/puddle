// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux: finds the block device under the data folder (from `/proc/self/mountinfo`) and under
//! each swap area (from `/proc/swaps`), then looks in `/sys` for a `dm-crypt` device at or below
//! it. `/sys` is readable by every user; nothing here needs root.
//!
//! A block device is encrypted when its device-mapper UUID starts with `CRYPT-` (LUKS, plain
//! dm-crypt) or when any device it is built on (`slaves/`, or a partition's parent disk) is, so
//! LVM on LUKS and LUKS on LVM both count. File-level encryption (fscrypt on one folder) is not
//! seen, except an eCryptfs mount.

use std::path::{Path, PathBuf};

use crate::facts::{DataEncryption, DiskEncryptionFacts, Protection, SwapArea, SwapFacts};

/// How many layers of devices are followed.
const MAX_DEPTH: usize = 8;

pub(crate) fn disk_encryption(dir: Option<&Path>) -> DiskEncryptionFacts {
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")
        .map(|t| parse_mounts(&t))
        .map_err(|e| format!("/proc/self/mountinfo: {e}"));
    let swaps = std::fs::read_to_string("/proc/swaps").map_err(|e| format!("/proc/swaps: {e}"));
    let wsl = is_wsl(&std::fs::read_to_string("/proc/version").unwrap_or_default());
    facts_from(Path::new("/sys"), dir, &mounts, &swaps, wsl)
}

fn facts_from(
    sys: &Path,
    dir: Option<&Path>,
    mounts: &Result<Vec<Mount>, String>,
    swaps: &Result<String, String>,
    wsl: bool,
) -> DiskEncryptionFacts {
    let mut facts = DiskEncryptionFacts {
        data: match dir {
            None => DataEncryption::NoFolder,
            Some(dir) => DataEncryption::Volume {
                dir: dir.to_path_buf(),
                protection: match mounts {
                    Ok(mounts) => path_protection(sys, mounts, &nearest_existing(dir)),
                    Err(why) => Protection::Unknown { why: why.clone() },
                },
            },
        },
        swap: match (swaps, mounts) {
            (Ok(swaps), Ok(mounts)) => swap_facts(sys, mounts, swaps),
            (Err(why), _) | (_, Err(why)) => SwapFacts::Unreadable { why: why.clone() },
        },
    };
    if wsl {
        wsl_unknown(&mut facts);
    }
    facts
}

/// `dir` or its nearest parent that exists (the data folder is made on first use), resolved.
fn nearest_existing(dir: &Path) -> PathBuf {
    dir.ancestors()
        .find_map(|p| p.canonicalize().ok())
        .unwrap_or_else(|| dir.to_path_buf())
}

fn is_wsl(proc_version: &str) -> bool {
    proc_version.to_ascii_lowercase().contains("microsoft")
}

/// Under WSL the disk is a virtual disk file on a Windows drive: nothing below it says whether
/// that drive is encrypted, so "not encrypted" would be a claim the probe cannot back.
fn wsl_unknown(facts: &mut DiskEncryptionFacts) {
    let relabel = |p: &Protection| {
        match p {
        Protection::NotEncrypted => Protection::Unknown {
            why: "this is WSL: its disk is a file on a Windows drive, so BitLocker on Windows decides; \
run puddle doctor in Windows"
                .to_owned(),
        },
        other => other.clone(),
    }
    };
    if let DataEncryption::Volume { protection, .. } = &mut facts.data {
        *protection = relabel(protection);
    }
    if let SwapFacts::Areas(areas) = &mut facts.swap {
        *areas = areas
            .iter()
            .map(|a| SwapArea {
                name: a.name.clone(),
                protection: relabel(&a.protection),
            })
            .collect();
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Mount {
    majmin: String,
    point: PathBuf,
    fstype: String,
    source: String,
}

/// `\040`-style escapes in a mountinfo or swaps field.
fn unescape(field: &str) -> String {
    let mut out = Vec::with_capacity(field.len());
    let mut rest = field.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        let octal = (b == b'\\')
            .then(|| tail.get(..3))
            .flatten()
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u8::from_str_radix(d, 8).ok());
        if let Some(byte) = octal {
            out.push(byte);
            rest = tail.get(3..).unwrap_or_default();
        } else {
            out.push(b);
            rest = tail;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let f: Vec<&str> = before.split(' ').collect();
            let mut a = after.split(' ');
            Some(Mount {
                majmin: (*f.get(2)?).to_owned(),
                point: PathBuf::from(unescape(f.get(4)?)),
                fstype: a.next()?.to_owned(),
                source: unescape(a.next()?),
            })
        })
        .collect()
}

/// The mount `path` is on: the longest mount point that holds it, the later line on a tie.
fn mount_of<'a>(mounts: &'a [Mount], path: &Path) -> Option<&'a Mount> {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.point))
        .max_by_key(|m| m.point.components().count())
}

fn path_protection(sys: &Path, mounts: &[Mount], path: &Path) -> Protection {
    let Some(mount) = mount_of(mounts, path) else {
        return Protection::Unknown {
            why: format!("no mount found for {}", path.display()),
        };
    };
    if mount.fstype == "ecryptfs" {
        return Protection::Encrypted {
            by: "eCryptfs".into(),
        };
    }
    let by_number = sys.join("dev/block").join(&mount.majmin);
    if by_number.exists() {
        return block_protection(&by_number);
    }
    // btrfs and similar report an anonymous device number: go by the source device instead.
    if Path::new(&mount.source).is_absolute()
        && let Ok(real) = Path::new(&mount.source).canonicalize()
        && let Some(name) = real.file_name()
    {
        let dir = sys.join("class/block").join(name);
        if dir.exists() {
            return block_protection(&dir);
        }
    }
    Protection::Unknown {
        why: format!(
            "{} is on a {} file system ({}), which is not a local block device",
            path.display(),
            mount.fstype,
            mount.source
        ),
    }
}

fn block_protection(dev: &Path) -> Protection {
    match crypt_below(dev, MAX_DEPTH) {
        Some(by) => Protection::Encrypted { by },
        None => Protection::NotEncrypted,
    }
}

/// The encryption method of `dev` or a device it sits on, if any.
fn crypt_below(dev: &Path, depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }
    if let Ok(uuid) = std::fs::read_to_string(dev.join("dm/uuid"))
        && let Some(rest) = uuid.trim().strip_prefix("CRYPT-")
    {
        return Some(match rest.split('-').next() {
            Some("LUKS1") => "LUKS".to_owned(),
            Some("LUKS2") => "LUKS2".to_owned(),
            _ => "dm-crypt".to_owned(),
        });
    }
    if let Ok(entries) = std::fs::read_dir(dev.join("slaves")) {
        for entry in entries.flatten() {
            if let Some(by) = crypt_below(&entry.path(), depth - 1) {
                return Some(by);
            }
        }
    }
    if dev.join("partition").exists()
        && let Ok(real) = dev.canonicalize()
        && let Some(parent) = real.parent()
    {
        return crypt_below(parent, depth - 1);
    }
    None
}

fn swap_facts(sys: &Path, mounts: &[Mount], swaps: &str) -> SwapFacts {
    let areas: Vec<SwapArea> = swaps
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let name = unescape(f.next()?);
            let kind = f.next()?;
            let protection = if name.starts_with("/dev/zram") {
                Protection::NotOnDisk {
                    what: "compressed memory (zram)".into(),
                }
            } else if kind == "partition" {
                match Path::new(&name)
                    .canonicalize()
                    .ok()
                    .and_then(|p| p.file_name().map(ToOwned::to_owned))
                {
                    Some(n) if sys.join("class/block").join(&n).exists() => {
                        block_protection(&sys.join("class/block").join(n))
                    }
                    _ => Protection::Unknown {
                        why: format!("{name} is not a block device in /sys"),
                    },
                }
            } else {
                path_protection(sys, mounts, Path::new(&name))
            };
            Some(SwapArea { name, protection })
        })
        .collect();
    if areas.is_empty() {
        SwapFacts::None
    } else {
        SwapFacts::Areas(areas)
    }
}

#[cfg(test)]
mod tests;
