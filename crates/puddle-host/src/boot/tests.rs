// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_workspace_gets_the_default_root_disk_or_the_size_it_was_grown_to_when_that_is_more() {
    assert_eq!(root_disk_size(None), DiskSize::gib(16));
    assert_eq!(root_disk_size(Some(4_096)), DiskSize::gib(16));
    assert_eq!(root_disk_size(Some(16_384)), DiskSize::gib(16));
    assert_eq!(root_disk_size(Some(40_960)), DiskSize::mib(40_960));
}

fn release(dir: &Path, version: &str) -> PathBuf {
    let release = dir.join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::write(release.join("bin/code-server"), "#!/bin/sh\n").unwrap();
    std::fs::write(
        release.join("code-server.json"),
        format!("{{\"version\": \"{version}\"}}"),
    )
    .unwrap();
    release
}

#[test]
fn dockers_disk_is_20_gib_at_var_lib_docker() {
    let disk = docker_disk();
    assert_eq!(disk.guest.as_str(), "/var/lib/docker");
    assert_eq!(disk.size, DiskSize::gib(20));
}

#[test]
fn the_release_is_installed_under_its_version_and_reused_when_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("share");
    std::fs::create_dir(&share).unwrap();
    let release = release(tmp.path(), "4.1.0");
    let target = install_code_server(&release, &share).unwrap();
    assert_eq!(target, share.join("code-server-4.1.0"));
    assert!(target.join("bin/code-server").is_file());
    assert!(target.join(INSTALLED_MARKER).is_file());
    // A second start finds it complete and leaves it alone.
    std::fs::write(target.join("sentinel"), "x").unwrap();
    assert_eq!(install_code_server(&release, &share).unwrap(), target);
    assert!(target.join("sentinel").is_file());
    // Another version sits beside it.
    let newer = release.with_file_name("newer");
    std::fs::create_dir(&newer).unwrap();
    std::fs::write(newer.join("code-server.json"), r#"{"version":"4.2.0"}"#).unwrap();
    assert_eq!(
        install_code_server(&newer, &share).unwrap(),
        share.join("code-server-4.2.0")
    );
    assert!(target.is_dir());
}

#[test]
fn older_releases_go_except_the_one_before_the_newest() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("share");
    std::fs::create_dir(&share).unwrap();
    let mut installed = Vec::new();
    for (n, version) in ["4.1.0", "4.2.0", "4.3.0", "4.4.0"].into_iter().enumerate() {
        let release = release(tmp.path(), version);
        installed.push(install_code_server(&release, &share).unwrap());
        // Installed one after the other, as releases are: the marker's time orders them.
        let marker = installed[n].join(INSTALLED_MARKER);
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000 * (n as u64 + 1));
        std::fs::File::options()
            .write(true)
            .open(&marker)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    // A fifth install drops everything older than the release before it, and only releases.
    std::fs::write(share.join("puddle-agent"), "agent").unwrap();
    std::fs::create_dir(share.join("code-server-9.9.9.partial")).unwrap();
    let fifth = install_code_server(&release(tmp.path(), "4.5.0"), &share).unwrap();
    assert!(fifth.is_dir());
    assert!(installed.iter().take(3).all(|old| !old.exists()));
    assert!(!share.join("code-server-9.9.9.partial").exists());
    assert!(installed[3].is_dir(), "the release before the newest stays");
    assert!(share.join("puddle-agent").is_file());
}

#[test]
fn cleaning_up_a_missing_folder_does_nothing_and_a_stray_file_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    prune_old_releases(&tmp.path().join("missing"), &tmp.path().join("keep"));
    std::fs::write(
        tmp.path().join("code-server-1.0.0"),
        "a file, not a release",
    )
    .unwrap();
    std::fs::create_dir(tmp.path().join("code-server-2.0.0")).unwrap();
    prune_old_releases(tmp.path(), &tmp.path().join("code-server-3.0.0"));
    // The folder without a marker counts as oldest and goes; one old release is kept.
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().flatten().collect();
    assert_eq!(left.len(), 1, "{left:?}");
}

#[cfg(unix)]
#[test]
fn a_release_that_cannot_be_removed_is_left_and_the_others_still_go() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let stuck = tmp.path().join("code-server-1.0.0");
    std::fs::create_dir_all(stuck.join("inner")).unwrap();
    std::fs::write(stuck.join("inner/file"), "x").unwrap();
    // flake-ok: a folder's mode, nothing is executed
    std::fs::set_permissions(stuck.join("inner"), std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root removes it anyway; the test is for a user the folder's mode binds.
    let binds = std::fs::write(stuck.join("inner/probe"), "").is_err();
    for name in ["code-server-2.0.0", "code-server-3.0.0"] {
        std::fs::create_dir(tmp.path().join(name)).unwrap();
    }
    prune_old_releases(tmp.path(), &tmp.path().join("code-server-3.0.0"));
    // flake-ok: a folder's mode, nothing is executed
    std::fs::set_permissions(stuck.join("inner"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!binds || stuck.join("inner/file").is_file());
    assert!(tmp.path().join("code-server-3.0.0").is_dir());
}

#[test]
fn a_half_installed_folder_is_made_again() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("share");
    std::fs::create_dir_all(share.join("code-server-4.1.0.partial")).unwrap();
    std::fs::create_dir_all(share.join("code-server-4.1.0")).unwrap();
    std::fs::write(share.join("code-server-4.1.0/stale"), "x").unwrap();
    let target = install_code_server(&release(tmp.path(), "4.1.0"), &share).unwrap();
    assert!(!target.join("stale").exists());
    assert!(target.join(INSTALLED_MARKER).is_file());
    assert!(!share.join("code-server-4.1.0.partial").exists());
}

#[test]
fn a_release_without_a_usable_version_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let share = tmp.path().join("share");
    std::fs::create_dir(&share).unwrap();
    let empty = tmp.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    assert!(install_code_server(&empty, &share).is_err());
    for bad in [
        "not json",
        r#"{"version": "../x"}"#,
        r#"{"version": ""}"#,
        "{}",
    ] {
        std::fs::write(empty.join("code-server.json"), bad).unwrap();
        let err = install_code_server(&empty, &share).unwrap_err().to_string();
        assert!(err.contains("no usable version"), "{bad}: {err}");
    }
}

#[test]
fn files_are_copied_where_a_link_is_not_possible() {
    let tmp = tempfile::tempdir().unwrap();
    let from = release(tmp.path(), "4.1.0");
    let to = tmp.path().join("copy");
    let no_links = |_: &Path, _: &Path| Err(std::io::Error::other("different disks"));
    link_or_copy_tree(&from, &to, &no_links).unwrap();
    assert_eq!(
        std::fs::read_to_string(to.join("bin/code-server")).unwrap(),
        "#!/bin/sh\n"
    );
}

#[test]
fn the_guest_mount_is_the_path_the_agent_is_told() {
    assert_eq!(
        puddle_boot::CODE_SERVER_GUEST,
        puddle_agent_proto::editor::GUEST_DIR
    );
}
