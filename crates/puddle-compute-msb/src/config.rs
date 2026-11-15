// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
