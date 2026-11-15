// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_probe_is_a_valid_static_x86_64_executable() {
    use object::{Object, ObjectSegment};
    let bytes = probe_x86_64();
    assert_eq!(bytes.len(), 132);
    let file = object::File::parse(&*bytes).unwrap();
    assert_eq!(file.architecture(), object::Architecture::X86_64);
    assert_eq!(file.kind(), object::ObjectKind::Executable);
    let seg = file.segments().next().unwrap();
    let entry = file.entry();
    assert!(entry >= seg.address() && entry < seg.address() + seg.size());
    // The code at the entry point sets the exit code to PROBE_EXIT_CODE.
    let at = usize::try_from(entry - seg.address()).unwrap();
    assert_eq!(&bytes[at..at + 2], &[0xbf, 42]);
}

#[test]
fn rootfs_holds_the_probe_and_the_root_user() {
    let dir = tempfile::tempdir().unwrap();
    write_rootfs(dir.path(), &probe_x86_64()).unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("probe")).unwrap(),
        probe_x86_64()
    );
    let passwd = std::fs::read_to_string(dir.path().join("etc/passwd")).unwrap();
    assert!(passwd.starts_with("root:x:0:0:"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join("probe"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}

#[test]
fn the_probe_is_chosen_by_guest_architecture() {
    assert_eq!(probe_for(GuestArch::X86_64), Some(probe_x86_64()));
    assert_eq!(probe_for(GuestArch::Aarch64), None);
    // Every architecture Puddle builds guests for has a probe.
    for arch in [GuestArch::X86_64, GuestArch::Aarch64] {
        assert_eq!(probe_for(arch).is_some(), arch.is_built(), "{arch}");
    }
}

#[test]
fn unknown_architectures_are_not_booted() {
    assert_eq!(
        test_boot(
            // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
            Path::new("/nonexistent"),
            "riscv64",
            Duration::from_secs(1),
            None
        ),
        BootFacts::UnsupportedArch {
            arch: "riscv64".into()
        }
    );
}

#[test]
fn other_architectures_are_not_booted() {
    assert_eq!(
        test_boot(
            // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
            Path::new("/nonexistent"),
            "aarch64",
            Duration::from_secs(1),
            None
        ),
        BootFacts::UnsupportedArch {
            arch: "aarch64".into()
        }
    );
}

#[test]
fn a_failed_boots_logs_are_kept_in_a_new_folder_that_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("sandboxes/puddle-doctor-boot/logs");
    std::fs::create_dir_all(logs.join("nested")).unwrap();
    std::fs::write(logs.join("runtime.log"), b"vmm").unwrap();
    std::fs::write(logs.join("kernel.log"), b"").unwrap();
    let keep = tempfile::tempdir().unwrap();
    let first = keep_boot_logs(home.path(), keep.path());
    let second = keep_boot_logs(home.path(), keep.path());
    // The folder names sort in the order the boots were kept.
    let mut kept: Vec<_> = std::fs::read_dir(keep.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    kept.sort();
    assert_eq!(kept.len(), 2, "{kept:?}");
    for (dir, reported) in kept.iter().zip([first, second]) {
        assert_eq!(
            reported,
            KeptLogs::Kept {
                dir: dir.clone(),
                failed: vec![]
            }
        );
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("puddle-doctor-boot-"), "{name}");
        assert_eq!(std::fs::read(dir.join("logs/runtime.log")).unwrap(), b"vmm");
        assert!(dir.join("logs/kernel.log").is_file());
        assert!(!dir.join("logs/nested").exists());
    }
}

fn kept_dir(kept: &KeptLogs) -> &Path {
    match kept {
        KeptLogs::Kept { dir, .. } => dir,
        KeptLogs::NotKept { detail } => Path::new(detail),
    }
}

fn kept_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn only_the_newest_three_kept_boots_stay() {
    let keep = tempfile::tempdir().unwrap();
    for stamp in [1005, 1001, 1004, 1002, 1003] {
        std::fs::create_dir_all(keep.path().join(format!("puddle-doctor-boot-{stamp}/logs")))
            .unwrap();
    }
    // A numbered variant of the same millisecond is newer than its base.
    std::fs::create_dir(keep.path().join("puddle-doctor-boot-1005-2")).unwrap();
    prune_kept(keep.path(), KEPT_BOOT_LOGS);
    assert_eq!(
        kept_names(keep.path()),
        [
            "puddle-doctor-boot-1004",
            "puddle-doctor-boot-1005",
            "puddle-doctor-boot-1005-2"
        ]
    );
}

#[test]
fn pruning_leaves_what_is_not_a_kept_boot_and_fewer_than_the_limit() {
    let keep = tempfile::tempdir().unwrap();
    std::fs::create_dir(keep.path().join("puddle-doctor-boot-1")).unwrap();
    std::fs::create_dir(keep.path().join("notes")).unwrap();
    std::fs::write(keep.path().join("puddle-doctor-boot-0"), b"a file").unwrap();
    prune_kept(keep.path(), 3);
    prune_kept(&keep.path().join("missing"), 3);
    assert_eq!(
        kept_names(keep.path()),
        ["notes", "puddle-doctor-boot-0", "puddle-doctor-boot-1"]
    );
}

#[test]
fn keeping_a_fourth_boot_removes_the_oldest() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("sandboxes/puddle-doctor-boot/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("runtime.log"), b"vmm").unwrap();
    let keep = tempfile::tempdir().unwrap();
    let kept: Vec<_> = (0..4)
        .map(|_| keep_boot_logs(home.path(), keep.path()))
        .collect();
    let names = kept_names(keep.path());
    assert_eq!(names.len(), KEPT_BOOT_LOGS, "{names:?}");
    assert!(!kept_dir(&kept[0]).exists());
    assert!(kept_dir(&kept[3]).join("logs/runtime.log").is_file());
}

#[cfg(unix)]
#[test]
fn the_test_boot_folder_is_owner_only_so_msb_accepts_it_as_a_catalog_ancestor() {
    use std::os::unix::fs::PermissionsExt;
    let work = work_dir().unwrap();
    let mode = std::fs::metadata(work.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o700, "{}", work.path().display());
}

#[cfg(unix)]
#[test]
fn the_keep_folders_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("sandboxes/puddle-doctor-boot/logs");
    std::fs::create_dir_all(&logs).unwrap();
    let data = tempfile::tempdir().unwrap();
    let keep = boot_logs_dir(data.path());
    let dir = kept_dir(&keep_boot_logs(home.path(), &keep)).to_path_buf();
    for folder in [&keep, &dir] {
        let mode = std::fs::metadata(folder).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{}", folder.display());
    }
}

#[test]
fn a_boot_without_logs_says_msb_wrote_none() {
    let home = tempfile::tempdir().unwrap();
    let keep = tempfile::tempdir().unwrap();
    let kept = keep_boot_logs(home.path(), keep.path());
    assert!(
        matches!(&kept, KeptLogs::NotKept { detail } if detail.contains("msb wrote no logs")),
        "{kept:?}"
    );
    assert_eq!(std::fs::read_dir(keep.path()).unwrap().count(), 0);
    // A boot that kept nothing has no folder, only the reason.
    assert!(
        kept_dir(&kept)
            .to_string_lossy()
            .contains("msb wrote no logs")
    );
}

#[test]
fn an_unreadable_log_folder_is_reported_with_its_error() {
    let home = tempfile::tempdir().unwrap();
    // A file where the logs folder should be: reading it as a folder fails, not "missing".
    let sandbox = home.path().join("sandboxes/puddle-doctor-boot");
    std::fs::create_dir_all(&sandbox).unwrap();
    std::fs::write(sandbox.join("logs"), b"not a folder").unwrap();
    let keep = tempfile::tempdir().unwrap();
    let kept = keep_boot_logs(home.path(), keep.path());
    assert!(
        matches!(&kept, KeptLogs::NotKept { detail }
                if detail.contains("logs") && !detail.contains("wrote no logs")),
        "{kept:?}"
    );
}

#[test]
fn a_keep_folder_that_cannot_be_made_is_reported_with_its_error() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("sandboxes/puddle-doctor-boot/logs");
    std::fs::create_dir_all(&logs).unwrap();
    std::fs::write(logs.join("runtime.log"), b"vmm").unwrap();
    let scratch = tempfile::tempdir().unwrap();
    // A file where the keep folder should be.
    let keep = scratch.path().join("kept");
    std::fs::write(&keep, b"in the way").unwrap();
    let kept = keep_boot_logs(home.path(), &keep);
    assert!(
        matches!(&kept, KeptLogs::NotKept { detail }
                if detail.contains("no folder to keep them in")
                    && detail.contains(&*keep.to_string_lossy())),
        "{kept:?}"
    );
}

#[test]
fn a_log_that_cannot_be_copied_is_listed_with_its_error() {
    let from = tempfile::tempdir().unwrap();
    std::fs::write(from.path().join("runtime.log"), b"vmm").unwrap();
    std::fs::create_dir(from.path().join("nested")).unwrap();
    // The target folder doesn't exist, so the one file can't be copied; the folder is skipped.
    let to = from.path().join("missing/logs");
    let failed = copy_logs(std::fs::read_dir(from.path()).unwrap(), from.path(), &to);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].contains("runtime.log"), "{failed:?}");
}

#[test]
fn a_keep_folder_name_that_is_taken_a_hundred_times_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..100 {
        new_folder(dir.path(), "x").unwrap();
    }
    let err = new_folder(dir.path(), "x").unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn a_keep_folder_that_cannot_be_created_is_an_error_not_a_retry() {
    let dir = tempfile::tempdir().unwrap();
    // The parent of the new folder doesn't exist: not "taken", so no other name is tried.
    let err = new_folder(dir.path(), "no-such-parent/x").unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn two_boots_in_one_millisecond_get_separate_folders() {
    let dir = tempfile::tempdir().unwrap();
    let a = new_folder(dir.path(), "x").unwrap();
    let b = new_folder(dir.path(), "x").unwrap();
    assert_ne!(a, b);
    assert!(b.ends_with("x-2"));
}

#[test]
fn the_command_runs_the_probe_with_a_private_home() {
    let layout = RuntimeLayout::new(
        std::path::absolute("rt").unwrap(),
        std::path::absolute("h").unwrap(),
    )
    .unwrap();
    let cmd = command(&layout, Path::new("/r"));
    let args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "run",
            "/r",
            "--name",
            "puddle-doctor-boot",
            "--log-level",
            "debug",
            "--no-stdin",
            "--",
            "/probe"
        ]
    );
    assert!(
        cmd.get_envs()
            .any(|(k, v)| k == "MSB_HOME" && v == Some(layout.home().as_os_str()))
    );
}
