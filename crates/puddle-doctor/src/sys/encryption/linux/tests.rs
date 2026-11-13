// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let m = parse_mounts("36 1 8:1 / /mnt/my\\040disk rw,noatime shared:1 - ext4 /dev/sda1 rw\n");
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
fn wsl_leaves_a_missing_folder_and_a_missing_swap_alone() {
    let mut facts = DiskEncryptionFacts {
        data: DataEncryption::NoFolder,
        swap: SwapFacts::None,
    };
    wsl_unknown(&mut facts);
    assert_eq!(facts.data, DataEncryption::NoFolder);
    assert_eq!(facts.swap, SwapFacts::None);
}

#[test]
fn the_probe_reads_this_machine_without_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let _facts = disk_encryption(Some(dir.path()));
    assert_eq!(disk_encryption(None).data, DataEncryption::NoFolder);
}
