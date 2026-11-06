// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use super::*;

    /// A fake `/sys/devices/<name>` block device with a `/sys/dev/block/<majmin>` link.
    fn device(sys: &Path, name: &str, majmin: &str, dm_uuid: Option<&str>) -> PathBuf {
        let dir = sys.join("devices").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(sys.join("dev/block")).unwrap();
        symlink(&dir, sys.join("dev/block").join(majmin)).unwrap();
        fs::create_dir_all(sys.join("class/block")).unwrap();
        symlink(&dir, sys.join("class/block").join(name)).unwrap();
        if let Some(uuid) = dm_uuid {
            fs::create_dir_all(dir.join("dm")).unwrap();
            fs::write(dir.join("dm/uuid"), format!("{uuid}\n")).unwrap();
        }
        dir
    }

    fn sits_on(upper: &Path, lower: &Path) {
        fs::create_dir_all(upper.join("slaves")).unwrap();
        symlink(lower, upper.join("slaves").join(lower.file_name().unwrap())).unwrap();
    }

    fn mount(majmin: &str, point: &str, fstype: &str, source: &str) -> String {
        format!("36 1 {majmin} / {point} rw - {fstype} {source} rw\n")
    }

    #[test]
    fn a_luks_device_under_the_mount_is_encrypted() {
        let sys = tempfile::tempdir().unwrap();
        let disk = device(sys.path(), "sda2", "8:2", None);
        let crypt = device(sys.path(), "dm-0", "253:0", Some("CRYPT-LUKS2-abc-root"));
        sits_on(&crypt, &disk);
        let mounts = parse_mounts(&mount("253:0", "/", "ext4", "/dev/mapper/root"));
        assert_eq!(
            path_protection(
                sys.path(),
                &mounts,
                // flake-ok: this module is built on Linux only; the path is matched against fake mountinfo text, never opened
                Path::new("/home/dev/.local/share/puddle")
            ),
            Protection::Encrypted { by: "LUKS2".into() }
        );
    }

    #[test]
    fn lvm_on_luks_counts_and_lvm_on_a_plain_disk_does_not() {
        let sys = tempfile::tempdir().unwrap();
        let disk = device(sys.path(), "sda2", "8:2", None);
        let luks = device(sys.path(), "dm-0", "253:0", Some("CRYPT-LUKS1-abc-crypt"));
        let lvm = device(sys.path(), "dm-1", "253:1", Some("LVM-xyz"));
        sits_on(&luks, &disk);
        sits_on(&lvm, &luks);
        let plain_disk = device(sys.path(), "sdb1", "8:17", None);
        let plain_lvm = device(sys.path(), "dm-2", "253:2", Some("LVM-plain"));
        sits_on(&plain_lvm, &plain_disk);
        assert_eq!(
            block_protection(&lvm),
            Protection::Encrypted { by: "LUKS".into() }
        );
        assert_eq!(block_protection(&plain_lvm), Protection::NotEncrypted);
    }

    #[test]
    fn a_partition_follows_its_disk_and_other_crypt_kinds_are_dm_crypt() {
        let sys = tempfile::tempdir().unwrap();
        let disk = device(sys.path(), "dm-5", "253:5", Some("CRYPT-PLAIN-x"));
        let part = disk.join("dm-5p1");
        fs::create_dir_all(&part).unwrap();
        fs::write(part.join("partition"), "1\n").unwrap();
        assert_eq!(
            block_protection(&part),
            Protection::Encrypted {
                by: "dm-crypt".into()
            }
        );
        assert_eq!(
            block_protection(&device(sys.path(), "sdc", "8:32", None)),
            Protection::NotEncrypted
        );
    }

    #[test]
    fn the_longest_mount_point_wins_and_odd_file_systems_are_unknown() {
        let sys = tempfile::tempdir().unwrap();
        let crypt = device(sys.path(), "dm-0", "253:0", Some("CRYPT-LUKS2-a"));
        let _plain = device(sys.path(), "sda1", "8:1", None);
        let _ = crypt;
        let text = [
            mount("8:1", "/", "ext4", "/dev/sda1"),
            mount("253:0", "/home", "ext4", "/dev/mapper/home"),
            mount("0:50", "/mnt/share", "nfs4", "server:/x"),
            mount("0:51", "/home/dev/crypt", "ecryptfs", "/home/dev/.Private"),
        ]
        .concat();
        let mounts = parse_mounts(&text);
        assert!(matches!(
            // flake-ok: this module is built on Linux only; the path is matched against fake mountinfo text, never opened
            path_protection(sys.path(), &mounts, Path::new("/home/dev/x")),
            Protection::Encrypted { .. }
        ));
        assert_eq!(
            path_protection(sys.path(), &mounts, Path::new("/srv")),
            Protection::NotEncrypted
        );
        assert!(matches!(
            path_protection(sys.path(), &mounts, Path::new("/mnt/share/a")),
            Protection::Unknown { .. }
        ));
        assert_eq!(
            // flake-ok: this module is built on Linux only; the path is matched against fake mountinfo text, never opened
            path_protection(sys.path(), &mounts, Path::new("/home/dev/crypt/a")),
            Protection::Encrypted {
                by: "eCryptfs".into()
            }
        );
        assert!(matches!(
            path_protection(sys.path(), &[], Path::new("/x")),
            Protection::Unknown { .. }
        ));
    }

    #[test]
    fn mountinfo_fields_are_unescaped() {
        let m =
            parse_mounts("36 1 8:1 / /mnt/my\\040disk rw,noatime shared:1 - ext4 /dev/sda1 rw\n");
        assert_eq!(m[0].point, PathBuf::from("/mnt/my disk"));
        assert_eq!(m[0].fstype, "ext4");
        assert_eq!(parse_mounts("garbage\n").len(), 0);
    }

    #[test]
    fn swap_partitions_files_and_zram() {
        let sys = tempfile::tempdir().unwrap();
        let _root = device(sys.path(), "sda1", "8:1", None);
        let mounts = parse_mounts(&mount("8:1", "/", "ext4", "/dev/sda1"));
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("swapfile");
        fs::write(&file, b"").unwrap();
        // A partition path that is not a block device in this fake /sys.
        let swaps = format!(
            "Filename\tType\tSize\tUsed\tPriority\n/dev/zram0 partition 100 0 100\n/nonexistent/part partition 1 0 -2\n{} file 1 0 -3\n",
            file.display()
        );
        let result = swap_facts(sys.path(), &mounts, &swaps);
        let areas = result.areas();
        assert!(matches!(areas[0].protection, Protection::NotOnDisk { .. }));
        assert!(matches!(areas[1].protection, Protection::Unknown { .. }));
        assert_eq!(areas[2].protection, Protection::NotEncrypted);
        assert_eq!(
            swap_facts(
                sys.path(),
                &mounts,
                "Filename\tType\tSize\tUsed\tPriority\n"
            ),
            SwapFacts::None
        );
    }

    #[test]
    fn wsl_turns_not_encrypted_into_unknown() {
        assert!(is_wsl("Linux version 6.6.114.1-microsoft-standard-WSL2"));
        assert!(!is_wsl("Linux version 6.8.0-generic"));
        let mut facts = DiskEncryptionFacts {
            data: DataEncryption::Volume {
                dir: "/d".into(),
                protection: Protection::NotEncrypted,
            },
            swap: SwapFacts::Areas(vec![SwapArea {
                name: "/swap".into(),
                protection: Protection::NotEncrypted,
            }]),
        };
        wsl_unknown(&mut facts);
        assert!(matches!(
            facts.data,
            DataEncryption::Volume {
                protection: Protection::Unknown { .. },
                ..
            }
        ));
        assert!(matches!(
            facts.swap.areas()[0].protection,
            Protection::Unknown { .. }
        ));
    }

    #[test]
    fn a_source_device_is_followed_when_the_device_number_is_anonymous() {
        let sys = tempfile::tempdir().unwrap();
        let _dev = device(sys.path(), "dm-0", "253:0", Some("CRYPT-LUKS2-a"));
        let nodes = tempfile::tempdir().unwrap();
        let node = nodes.path().join("dm-0");
        fs::write(&node, b"").unwrap();
        let text = mount("0:44", "/", "btrfs", &node.display().to_string());
        let mounts = parse_mounts(&text);
        assert!(matches!(
            path_protection(sys.path(), &mounts, Path::new("/")),
            Protection::Encrypted { .. }
        ));
        let swaps = format!("Filename Type\n{} partition 1 0 -2\n", node.display());
        let result = swap_facts(sys.path(), &mounts, &swaps);
        assert!(matches!(
            result.areas()[0].protection,
            Protection::Encrypted { .. }
        ));
    }

    #[test]
    fn unreadable_inputs_say_unknown_and_the_walk_is_bounded() {
        let sys = tempfile::tempdir().unwrap();
        let dir = Path::new("/");
        let facts = facts_from(
            sys.path(),
            Some(dir),
            &Err("m".into()),
            &Ok(String::new()),
            false,
        );
        assert!(matches!(
            facts.data,
            DataEncryption::Volume {
                protection: Protection::Unknown { .. },
                ..
            }
        ));
        assert_eq!(facts.swap, SwapFacts::Unreadable { why: "m".into() });
        let facts = facts_from(sys.path(), None, &Ok(vec![]), &Err("s".into()), false);
        assert_eq!(facts.swap, SwapFacts::Unreadable { why: "s".into() });
        let nodes = tempfile::tempdir().unwrap();
        let orphan = nodes.path().join("not-in-sys");
        fs::write(&orphan, b"").unwrap();
        let mounts = Ok(parse_mounts(&mount(
            "0:45",
            "/",
            "btrfs",
            &orphan.display().to_string(),
        )));
        let swaps = Ok("Filename Type\n/dev/zram0 partition 1 0 -2\n".to_owned());
        let facts = facts_from(sys.path(), Some(dir), &mounts, &swaps, true);
        assert_eq!(facts.swap.areas().len(), 1);
        assert!(matches!(
            facts.data,
            DataEncryption::Volume {
                protection: Protection::Unknown { .. },
                ..
            }
        ));
        let plain = device(sys.path(), "sdz", "8:99", None);
        assert_eq!(crypt_below(&plain, 0), None);
    }

    #[test]
    fn the_probe_reads_this_machine_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let _facts = disk_encryption(Some(dir.path()));
        assert_eq!(disk_encryption(None).data, DataEncryption::NoFolder);
    }
}
