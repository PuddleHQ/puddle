// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn m(name: &str, status: WorkspaceStatus, volumes: &[&str]) -> Mounter {
    Mounter {
        name: name.into(),
        status,
        volumes: volumes.iter().map(|v| (*v).to_owned()).collect(),
    }
}

#[test]
fn only_running_sandboxes_hold_volumes() {
    let h = holders(&[
        m("stopped", WorkspaceStatus::Stopped, &["ws-a"]),
        m("crashed", WorkspaceStatus::Crashed, &["ws-b"]),
        m("running", WorkspaceStatus::Running, &["ws-a", "ws-c"]),
    ]);
    assert_eq!(h.get("ws-a").map(String::as_str), Some("running"));
    assert_eq!(h.get("ws-c").map(String::as_str), Some("running"));
    assert_eq!(h.get("ws-b"), None);
}

#[test]
fn two_running_holders_report_the_first_by_name() {
    let h = holders(&[
        m("zeta", WorkspaceStatus::Running, &["ws-a"]),
        m("alpha", WorkspaceStatus::Running, &["ws-a"]),
    ]);
    assert_eq!(h.get("ws-a").map(String::as_str), Some("alpha"));
}

#[test]
fn use_is_rounded_up_to_whole_mib() {
    assert_eq!(used_mib(0), DiskSize::mib(0));
    assert_eq!(used_mib(1), DiskSize::mib(1));
    assert_eq!(used_mib(1024 * 1024), DiskSize::mib(1));
    assert_eq!(used_mib(1024 * 1024 + 1), DiskSize::mib(2));
    assert_eq!(used_mib(u64::MAX), DiskSize::mib(u32::MAX));
}

#[test]
fn a_volume_reads_as_what_its_image_holds_and_unknown_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("disk.raw");
    std::fs::write(&image, vec![1_u8; 2 * 1024 * 1024]).unwrap();
    let used = used_of("ws-a", Some(image)).unwrap();
    assert!(used >= DiskSize::mib(2), "{used:?}");
    assert!(used <= DiskSize::mib(3), "{used:?}");
    // A directory volume has no image; an image that cannot be looked at is unknown, not zero.
    assert_eq!(used_of("ws-a", None), None);
    assert_eq!(used_of("ws-a", Some(dir.path().join("missing.raw"))), None);
}

#[test]
fn sizes_come_from_capacity_then_quota() {
    assert_eq!(size_mib(Some(256 * 1024 * 1024), None), DiskSize::mib(256));
    assert_eq!(
        size_mib(Some(256 * 1024 * 1024 + 5), Some(7)),
        DiskSize::mib(256)
    );
    assert_eq!(size_mib(None, Some(7)), DiskSize::mib(7));
    assert_eq!(size_mib(None, None), DiskSize::mib(0));
    assert_eq!(size_mib(Some(u64::MAX), None), DiskSize::mib(u32::MAX));
}

fn bad_config() -> Result<SandboxConfig, String> {
    Err("missing field `spec`".to_owned())
}

#[test]
fn an_unreadable_config_of_a_running_sandbox_is_an_error_naming_it() {
    for status in [WorkspaceStatus::Running, WorkspaceStatus::Starting] {
        let err = mounter("ws-a", status, bad_config())
            .unwrap_err()
            .to_string();
        assert!(err.contains("`ws-a`"), "{err}");
        assert!(err.contains("missing field `spec`"), "{err}");
        assert!(err.contains("remove the sandbox"), "{err}");
    }
}

#[test]
fn an_unreadable_config_of_a_stopped_sandbox_holds_nothing() {
    let m = mounter("ws-a", WorkspaceStatus::Stopped, bad_config()).unwrap();
    assert_eq!(m.volumes, Vec::<String>::new());
}

#[test]
fn a_readable_config_of_a_stopped_sandbox_keeps_its_volumes() {
    let mut config = SandboxConfig::default();
    config.spec.mounts = vec![VolumeMount::Named {
        name: "ws-a".into(),
        guest: "/w".into(),
        create: None,
        options: microsandbox::sandbox::MountOptions::default(),
        stat_virtualization: microsandbox::sandbox::StatVirtualization::Strict,
        host_permissions: microsandbox::sandbox::HostPermissions::Private,
        follow_root_symlinks: false,
    }];
    let m = mounter("s", WorkspaceStatus::Stopped, Ok::<_, String>(config)).unwrap();
    assert_eq!(m.volumes, ["ws-a"]);
}

#[test]
fn mounted_volumes_fail_on_an_unreadable_config() {
    let err = mounted_volumes("ws-a", bad_config())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("`ws-a`") && err.contains("single-writer"),
        "{err}"
    );
    assert_eq!(
        mounted_volumes("ws-a", Ok::<_, String>(SandboxConfig::default())).unwrap(),
        Vec::<String>::new()
    );
}

#[test]
fn named_volumes_are_read_from_the_config() {
    let mut config = SandboxConfig::default();
    config.spec.mounts = vec![
        VolumeMount::Named {
            name: "ws-a".into(),
            guest: "/w".into(),
            create: None,
            options: microsandbox::sandbox::MountOptions::default(),
            stat_virtualization: microsandbox::sandbox::StatVirtualization::Strict,
            host_permissions: microsandbox::sandbox::HostPermissions::Private,
            follow_root_symlinks: false,
        },
        VolumeMount::Bind {
            host: "/h".into(),
            guest: "/b".into(),
            options: microsandbox::sandbox::MountOptions::default(),
            stat_virtualization: microsandbox::sandbox::StatVirtualization::Strict,
            host_permissions: microsandbox::sandbox::HostPermissions::Private,
            follow_root_symlinks: false,
            quota_mib: None,
        },
    ];
    assert_eq!(named_volumes(&config), ["ws-a"]);
}
