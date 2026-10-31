// SPDX-License-Identifier: GPL-3.0-or-later
//! Where the adapter's msb world lives, and the mount-source rule.

use std::path::{Path, PathBuf};
use std::time::Duration;

use microsandbox::sandbox::{SandboxConfig, VolumeMount};
use puddle_compute::{ComputeError, MountKind};

/// The SDK's SSH server settings for [`puddle_compute::Sandbox::serve_ssh`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshConfig {
    /// OpenSSH public key lines that may log in. The SDK refuses to serve without one.
    pub authorized_keys: Vec<String>,
    /// Session inactivity timeout; `None` turns it off (IDE sessions sit idle for long).
    pub inactivity_timeout: Option<Duration>,
}

/// The proxy the registry client sends image pulls through, as a URL that may carry credentials
/// (`http://puddle:<token>@127.0.0.1:<port>`). `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct RegistryProxy(String);

impl RegistryProxy {
    /// The URL, credentials included. Only for handing to the SDK.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for RegistryProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RegistryProxy(<redacted>)")
    }
}

/// Everything [`crate::MsbRuntime::open`] needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MsbConfig {
    /// Puddle's private msb home (database, image cache, sandboxes, volumes, logs).
    pub home: PathBuf,
    /// The bundled `msb` binary.
    pub msb: PathBuf,
    /// The `libkrunfw` library released with it.
    pub libkrunfw: PathBuf,
    /// The only directory file mounts may come from: it holds just the files meant for guests,
    /// never Puddle's data (database, tokens, CA keys). The one exception is a checkout's `.git`
    /// ([`MountKind::Checkout`]), which has a rule of its own.
    pub guest_share: PathBuf,
    /// Folders no checkout mount may lie in or hold, besides the msb home and the guest share:
    /// Puddle's data folder.
    pub protected_dirs: Vec<PathBuf>,
    /// SSH server settings.
    pub ssh: SshConfig,
    /// Extra roots the registry client trusts for image pulls, as PEM: the corporate roots
    /// (`puddle-certs`), so pulls work behind a TLS-intercepting company proxy.
    /// Added to the platform's roots, never instead of them.
    pub registry_roots: Vec<String>,
    /// Where image pulls go: Puddle's pull proxy. Given to the SDK's registry client
    /// directly, so the token is never in the process environment. `None` leaves pulls
    /// to the process environment, which is what tests without a pull proxy want.
    pub registry_proxy: Option<RegistryProxy>,
    /// Default log level of msb's sandbox runtimes (`runtime.log`, msb's `log_level`): `error`,
    /// `warn`, `info`, `debug` or `trace`. [`DEFAULT_RUNTIME_LOG_LEVEL`] unless changed, so a rare
    /// boot failure in the field leaves the VMM's trace. `None` leaves msb's own default, which
    /// is silent. The VM tests set `debug`.
    pub runtime_log_level: Option<String>,
    /// Where [`crate::MsbRuntime`] copies a sandbox's `logs/` directory to before it removes the
    /// sandbox, as `<dir>/<sandbox>/logs/`. `None` (the default) keeps nothing: removing a sandbox
    /// deletes its logs. The VM tests set it so a failed boot keeps its full logs as CI artifacts.
    pub keep_logs_dir: Option<PathBuf>,
}

/// The default [`MsbConfig::runtime_log_level`]. `info` is a few lines per boot; msb rotates
/// `runtime.log` at 10 MiB on every platform, keeping a few rotated files.
pub const DEFAULT_RUNTIME_LOG_LEVEL: &str = "info";

impl MsbConfig {
    /// A config for msb home `home`, runtime pair `msb` + `libkrunfw` and guest-share root
    /// `guest_share`, with no SSH keys.
    #[must_use]
    pub fn new(
        home: impl Into<PathBuf>,
        msb: impl Into<PathBuf>,
        libkrunfw: impl Into<PathBuf>,
        guest_share: impl Into<PathBuf>,
    ) -> Self {
        Self {
            home: home.into(),
            msb: msb.into(),
            libkrunfw: libkrunfw.into(),
            guest_share: guest_share.into(),
            protected_dirs: Vec::new(),
            ssh: SshConfig::default(),
            registry_roots: Vec::new(),
            registry_proxy: None,
            runtime_log_level: Some(DEFAULT_RUNTIME_LOG_LEVEL.to_owned()),
            keep_logs_dir: None,
        }
    }

    /// The same config, with msb's sandbox runtimes logging at `level`; `None` keeps the level
    /// the config has (by default [`DEFAULT_RUNTIME_LOG_LEVEL`]), so a test can pass an optional
    /// override straight through.
    #[must_use]
    pub fn with_runtime_log_level(mut self, level: Option<impl Into<String>>) -> Self {
        if let Some(level) = level {
            self.runtime_log_level = Some(level.into());
        }
        self
    }

    /// The same config, keeping a removed sandbox's `logs/` under `dir` (see
    /// [`MsbConfig::keep_logs_dir`]).
    #[must_use]
    pub fn with_keep_logs_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.keep_logs_dir = Some(dir.into());
        self
    }

    /// The same config, with image pulls also trusting `roots` (PEM, one or more certificates
    /// each).
    #[must_use]
    pub fn with_registry_roots(mut self, roots: impl IntoIterator<Item = String>) -> Self {
        self.registry_roots.extend(roots);
        self
    }

    /// The same config, with image pulls sent through the proxy at `url` (credentials allowed).
    /// The SDK's registry client takes it as its own setting and ignores the proxy variables of
    /// the process environment for those pulls.
    #[must_use]
    pub fn with_registry_proxy(mut self, url: impl Into<String>) -> Self {
        self.registry_proxy = Some(RegistryProxy(url.into()));
        self
    }

    /// The same config, refusing a checkout mount inside `dir` or holding it (see
    /// [`MsbConfig::protected_dirs`]).
    #[must_use]
    pub fn with_protected_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.protected_dirs.push(dir.into());
        self
    }

    /// The same config, accepting SSH logins with `key` (an OpenSSH public key line).
    #[must_use]
    pub fn with_ssh_key(mut self, key: impl Into<String>) -> Self {
        self.ssh.authorized_keys.push(key.into());
        self
    }

    /// msb's `config.json` in the home: the runtime pair, written by
    /// [`crate::MsbRuntime::open`].
    #[must_use]
    pub fn config_path(&self) -> PathBuf {
        self.home.join("config.json")
    }

    /// The `config.json` contents: version 1, the two runtime paths and, when set, the
    /// runtimes' `log_level`; nothing else, so every other setting is the SDK default listed in
    /// [`crate::SDK_OPTIONS`].
    #[must_use]
    pub fn config_json(&self) -> String {
        let mut config = serde_json::Map::new();
        config.insert("version".into(), 1.into());
        config.insert(
            "paths".into(),
            serde_json::json!({ "msb": self.msb, "libkrunfw": self.libkrunfw }),
        );
        if let Some(level) = &self.runtime_log_level {
            config.insert("log_level".into(), level.as_str().into());
        }
        serde_json::Value::Object(config).to_string()
    }

    /// Checks that `host` lies under [`MsbConfig::guest_share`] after resolving symlinks and
    /// `..`, so a mount can never expose Puddle's own data. The share root itself is refused: a
    /// directory mount names a folder below it. A [`MountKind::Checkout`] instead must be the
    /// canonical path of a real `.git` directory outside the msb home, the guest share and every
    /// [`MsbConfig::protected_dirs`] folder, owned by this user on Unix.
    ///
    /// # Errors
    ///
    /// [`ComputeError::InvalidSpec`] when it doesn't, when either path can't be resolved, or
    /// when it isn't a file (or a directory, for [`MountKind::Dir`]) as the mount says.
    pub fn check_mount_source(&self, host: &Path, kind: MountKind) -> Result<(), ComputeError> {
        match kind {
            MountKind::Checkout => self.check_checkout(host),
            MountKind::File => self.check_shared(host, true),
            MountKind::Dir => self.check_shared(host, false),
        }
    }

    /// The checkout rule (see `crate::checkout`), with the msb home, the guest share and every
    /// [`MsbConfig::protected_dirs`] folder protected.
    ///
    /// # Errors
    ///
    /// [`ComputeError::InvalidSpec`] naming what is wrong with the folder.
    pub fn check_checkout(&self, host: &Path) -> Result<(), ComputeError> {
        // Without the data folder named, a checkout inside it would pass: fail closed.
        if self.protected_dirs.is_empty() {
            return Err(ComputeError::InvalidSpec {
                reason: format!(
                    "the checkout {} can't be mounted: Puddle's data folder was not given to the runtime",
                    host.display()
                ),
            });
        }
        let protected: Vec<&Path> = [self.home.as_path(), self.guest_share.as_path()]
            .into_iter()
            .chain(self.protected_dirs.iter().map(PathBuf::as_path))
            .collect();
        crate::checkout::check(host, &protected)
    }

    /// The guest-share rule for a file (`file`) or directory mount.
    fn check_shared(&self, host: &Path, file: bool) -> Result<(), ComputeError> {
        let invalid = |reason: String| ComputeError::InvalidSpec { reason };
        let root = self.guest_share.canonicalize().map_err(|e| {
            invalid(format!(
                "guest-share root {} can't be resolved: {e}",
                self.guest_share.display()
            ))
        })?;
        let source = host.canonicalize().map_err(|e| {
            invalid(format!(
                "mount source {} can't be resolved: {e}",
                host.display()
            ))
        })?;
        if !source.starts_with(&root) || source == root {
            return Err(invalid(format!(
                "mount source {} is outside the guest-share root {}",
                host.display(),
                self.guest_share.display()
            )));
        }
        let (found, want) = if file {
            (source.is_file(), "file")
        } else {
            (source.is_dir(), "directory")
        };
        if found {
            Ok(())
        } else {
            Err(invalid(format!(
                "mount source {} is not a {want}",
                host.display()
            )))
        }
    }
}

impl MsbConfig {
    /// Refuses to start sandbox `name` again when its stored configuration binds a host path
    /// outside the guest share: only a checkout mount does, and a sandbox with one is made for
    /// the one boot of its create. Its source was checked at that create; a start would bind
    /// whatever the path holds by now, unchecked. A stored configuration that can't be read
    /// passes here: the volume check that follows refuses it with its own reason.
    pub(crate) fn check_restart<E>(
        &self,
        name: &str,
        stored: Result<SandboxConfig, E>,
    ) -> Result<(), ComputeError> {
        let Ok(config) = stored else {
            return Ok(());
        };
        let root = self.guest_share.canonicalize().ok();
        for mount in &config.spec.mounts {
            let VolumeMount::Bind { host, .. } = mount else {
                continue;
            };
            let inside = match (host.canonicalize(), &root) {
                (Ok(source), Some(root)) => source.starts_with(root),
                // A source that is gone: judged by its spelling, which a start then fails on.
                _ => {
                    host.starts_with(&self.guest_share)
                        || root.as_ref().is_some_and(|r| host.starts_with(r))
                }
            };
            if !inside {
                return Err(ComputeError::InvalidSpec {
                    reason: format!(
                        "sandbox `{name}` mounts {}, which is outside the guest share (a checkout); \
                         a sandbox that mounts a checkout is only for its create and is never started again; remove it",
                        host.display()
                    ),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
