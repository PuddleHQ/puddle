// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(&format!("puddle-msb-config-{tag}-"))
                .tempdir()
                .unwrap()
                .keep(),
        )
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_registry_proxy_is_kept_but_never_shown_or_written() {
    let url = "http://puddle:t0k3n@127.0.0.1:4000";
    let plain = MsbConfig::new("/h", "/rt/msb", "/rt/libkrunfw.so", "/share");
    assert_eq!(plain.registry_proxy, None);
    let c = plain.clone().with_registry_proxy(url);
    assert_eq!(
        c.registry_proxy.as_ref().map(RegistryProxy::expose),
        Some(url)
    );
    assert!(!format!("{c:?}").contains("t0k3n"), "{c:?}");
    assert_eq!(c.config_json(), plain.config_json());
}

#[test]
fn config_json_pins_only_the_runtime_pair_and_the_log_level() {
    let mut c = MsbConfig::new("/h", "/rt/msb", "/rt/libkrunfw.so", "/share");
    c.runtime_log_level = None;
    let v: serde_json::Value = serde_json::from_str(&c.config_json()).unwrap();
    assert_eq!(v["version"], 1);
    assert_eq!(v["paths"]["msb"], "/rt/msb");
    assert_eq!(v["paths"]["libkrunfw"], "/rt/libkrunfw.so");
    assert_eq!(v.as_object().unwrap().len(), 2);
    assert_eq!(c.config_path(), PathBuf::from("/h").join("config.json"));
}

#[test]
fn the_runtime_log_level_defaults_to_info_and_none_keeps_what_is_set() {
    let plain = MsbConfig::new("/h", "/m", "/l", "/s");
    assert_eq!(plain.runtime_log_level.as_deref(), Some("info"));
    let v: serde_json::Value = serde_json::from_str(&plain.config_json()).unwrap();
    assert_eq!(v["log_level"], "info");
    let c = plain.clone().with_runtime_log_level(Some("debug"));
    let v: serde_json::Value = serde_json::from_str(&c.config_json()).unwrap();
    assert_eq!(v["log_level"], "debug");
    assert_eq!(v["paths"]["msb"], "/m");
    let same = c.clone().with_runtime_log_level(None::<String>);
    assert_eq!(same.config_json(), c.config_json());
    let mut silent = plain;
    silent.runtime_log_level = None;
    let v: serde_json::Value = serde_json::from_str(&silent.config_json()).unwrap();
    assert!(v.get("log_level").is_none());
}

#[test]
fn kept_logs_are_off_unless_a_directory_is_given() {
    let c = MsbConfig::new("/h", "/m", "/l", "/s");
    assert_eq!(c.keep_logs_dir, None);
    let c = c.with_keep_logs_dir("/kept");
    assert_eq!(c.keep_logs_dir, Some(PathBuf::from("/kept")));
}

#[test]
fn ssh_keys_accumulate_and_the_timeout_is_off_by_default() {
    let c = MsbConfig::new("/h", "/m", "/l", "/s")
        .with_ssh_key("ssh-ed25519 AAAA a")
        .with_ssh_key("ssh-ed25519 BBBB b");
    assert_eq!(c.ssh.authorized_keys.len(), 2);
    assert_eq!(c.ssh.inactivity_timeout, None);
}

#[test]
fn mount_sources_must_be_inside_the_guest_share_root() {
    let dir = Dir::new("mounts");
    let share = dir.0.join("guest-share");
    let data = dir.0.join("data");
    std::fs::create_dir_all(share.join("boot")).unwrap();
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(share.join("boot").join("boot.sh"), b"x").unwrap();
    std::fs::write(data.join("puddle.db"), b"x").unwrap();
    let c = MsbConfig::new("/h", "/m", "/l", &share);

    c.check_mount_source(&share.join("boot").join("boot.sh"), MountKind::File)
        .unwrap();
    let outside = c
        .check_mount_source(&data.join("puddle.db"), MountKind::File)
        .unwrap_err();
    assert!(
        outside.to_string().contains("outside the guest-share root"),
        "{outside}"
    );
    // `..` can't climb out: the path is resolved first.
    let climb = share
        .join("boot")
        .join("..")
        .join("..")
        .join("data")
        .join("puddle.db");
    assert!(c.check_mount_source(&climb, MountKind::File).is_err());
    // The root itself isn't a file meant for a guest.
    assert!(c.check_mount_source(&share, MountKind::File).is_err());
    assert!(c.check_mount_source(&share, MountKind::Dir).is_err());
    // A missing source or root is refused, not assumed fine.
    let missing = c
        .check_mount_source(&share.join("nope"), MountKind::File)
        .unwrap_err();
    assert!(
        missing.to_string().contains("can't be resolved"),
        "{missing}"
    );
    // The mount's kind must match what is on disk.
    let wrong = c
        .check_mount_source(&share.join("boot"), MountKind::File)
        .unwrap_err();
    assert!(wrong.to_string().contains("is not a file"), "{wrong}");
    c.check_mount_source(&share.join("boot"), MountKind::Dir)
        .unwrap();
    let wrong = c
        .check_mount_source(&share.join("boot").join("boot.sh"), MountKind::Dir)
        .unwrap_err();
    assert!(wrong.to_string().contains("is not a directory"), "{wrong}");
    let no_root = MsbConfig::new("/h", "/m", "/l", dir.0.join("absent"));
    assert!(
        no_root
            .check_mount_source(&data.join("puddle.db"), MountKind::File)
            .is_err()
    );
}

#[test]
fn a_checkout_mount_follows_its_own_rule_with_every_puddle_folder_protected() {
    let dir = Dir::new("checkout");
    let base = dunce::canonicalize(&dir.0).unwrap();
    let (home, share, data) = (base.join("msb"), base.join("share"), base.join("data"));
    let git = base.join("api").join(".git");
    for d in [
        &home,
        &share,
        &data,
        &git.join("objects"),
        &git.join("refs"),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let c = MsbConfig::new(&home, "/m", "/l", &share).with_protected_dir(&data);
    assert_eq!(c.protected_dirs, vec![data.clone()]);
    c.check_mount_source(&git, MountKind::Checkout).unwrap();
    // The same directory is no plain directory mount: it is not in the share.
    assert!(c.check_mount_source(&git, MountKind::Dir).is_err());
    // Each of the three folders is protected, the data folder through the config.
    for root in [&home, &share, &data] {
        let inside = root.join("api").join(".git");
        for d in [inside.join("objects"), inside.join("refs")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(inside.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let err = c
            .check_mount_source(&inside, MountKind::Checkout)
            .unwrap_err();
        assert!(
            err.to_string().contains("lies inside Puddle's folder"),
            "{err}"
        );
    }
    // Without a data folder named, every checkout is refused.
    let unprotected = MsbConfig::new(&home, "/m", "/l", &share);
    let err = unprotected
        .check_mount_source(&git, MountKind::Checkout)
        .unwrap_err();
    assert!(
        err.to_string().contains("data folder was not given"),
        "{err}"
    );
}

#[test]
fn a_sandbox_that_binds_a_path_outside_the_share_is_never_started_again() {
    let dir = Dir::new("restart");
    let share = dir.0.join("share");
    std::fs::create_dir_all(share.join("boot")).unwrap();
    std::fs::create_dir_all(dir.0.join("api").join(".git")).unwrap();
    let c = MsbConfig::new("/h", "/m", "/l", &share);
    let bind = |host: PathBuf| VolumeMount::Bind {
        host,
        guest: "/g".into(),
        options: microsandbox::sandbox::MountOptions::default(),
        stat_virtualization: microsandbox::sandbox::StatVirtualization::Strict,
        host_permissions: microsandbox::sandbox::HostPermissions::Private,
        follow_root_symlinks: false,
        quota_mib: None,
    };
    let mut config = SandboxConfig::default();
    let named = VolumeMount::Named {
        name: "ws-a".into(),
        guest: "/w".into(),
        create: None,
        options: microsandbox::sandbox::MountOptions::default(),
        stat_virtualization: microsandbox::sandbox::StatVirtualization::Strict,
        host_permissions: microsandbox::sandbox::HostPermissions::Private,
        follow_root_symlinks: false,
    };
    config.spec.mounts = vec![
        named,
        bind(share.join("boot")),
        bind(share.join("gone-file")),
    ];
    c.check_restart("ws", Ok::<_, String>(config.clone()))
        .unwrap();
    config
        .spec
        .mounts
        .push(bind(dir.0.join("api").join(".git")));
    let err = c
        .check_restart("ws", Ok::<_, String>(config.clone()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("never started again"), "{err}");
    // A source that is gone and was never in the share is refused too.
    config.spec.mounts = vec![bind(dir.0.join("gone").join(".git"))];
    assert!(
        c.check_restart("ws", Ok::<_, String>(config.clone()))
            .is_err()
    );
    // A stored configuration that can't be read is left to the volume check after it.
    c.check_restart("ws", Err::<SandboxConfig, _>("unreadable"))
        .unwrap();
    // `..` out of the share is resolved, not taken by its spelling.
    config.spec.mounts = vec![bind(share.join("..").join("api").join(".git"))];
    assert!(
        c.check_restart("ws", Ok::<_, String>(config.clone()))
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_in_the_share_cannot_point_outside() {
    let dir = Dir::new("symlink");
    let share = dir.0.join("guest-share");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(dir.0.join("secret"), b"x").unwrap();
    std::os::unix::fs::symlink(dir.0.join("secret"), share.join("link")).unwrap();
    let c = MsbConfig::new("/h", "/m", "/l", &share);
    assert!(
        c.check_mount_source(&share.join("link"), MountKind::File)
            .is_err()
    );
}
