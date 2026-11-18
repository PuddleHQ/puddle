// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real machine as a [`Probe`].

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_runtime::{
    BundledRuntime, DevOverride, DigestStatus, RUNTIME_DIR_NAME, RuntimeDigests, RuntimeError,
    RuntimeLayout, RuntimeVersion, VersionStatus,
};

use puddle_ssh_agent::{
    AgentEndpoint, AgentError, Identity, WINDOWS_DEFAULT_PIPE, key_family, list_identities,
};

use crate::diagnose::Probe;
use crate::facts::{
    AccessDenied, AgentService, AzCliFacts, BootFacts, CodeIntegrityFacts, CredentialStoreFacts,
    DataPathFacts, DiskEncryptionFacts, DiskFacts, GsaFacts, HypervisorFacts, JobFacts, Os,
    ProcessOutcome, RuntimeFacts, RuntimeState, SealedKeyFacts, SshAgentFacts, TpmFacts,
    VaultFacts,
};
use crate::{boot, launch, sys};

/// Probes this machine and the bundled runtime in one folder.
#[derive(Debug)]
pub struct SystemProbe {
    runtime_dir: PathBuf,
    expected: RuntimeVersion,
    dev: DevOverride,
    digests: RuntimeDigests,
    keep_boot_logs: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    ssh_agent_pipe: Option<String>,
    az_search_path: Option<std::ffi::OsString>,
    sealed_key_search: (Option<PathBuf>, Option<std::ffi::OsString>),
    opened: OnceCell<BundledRuntime>,
}

impl SystemProbe {
    /// Probes the runtime in `runtime_dir`, which must be exactly `expected` (unless `dev` is
    /// active in a development build).
    #[must_use]
    pub fn new(runtime_dir: PathBuf, expected: RuntimeVersion, dev: DevOverride) -> Self {
        Self {
            runtime_dir,
            expected,
            dev,
            digests: RuntimeDigests::built_in(),
            keep_boot_logs: None,
            data_dir: None,
            ssh_agent_pipe: None,
            az_search_path: None,
            sealed_key_search: (puddle_secrets::config_home(), std::env::var_os("PATH")),
            opened: OnceCell::new(),
        }
    }

    /// Checks the runtime's files against `digests` instead of the hashes built into this
    /// Puddle (tests with fake runtime folders).
    #[must_use]
    pub fn with_digests(mut self, digests: RuntimeDigests) -> Self {
        self.digests = digests;
        self
    }

    /// Keeps msb's logs of a failed test boot in a new folder inside `dir` (see
    /// [`boot::test_boot`]). Unset, a failed boot's logs go with its throwaway home.
    #[must_use]
    pub fn with_keep_boot_logs(mut self, dir: impl Into<PathBuf>) -> Self {
        self.keep_boot_logs = Some(dir.into());
        self
    }

    /// Measures the free space of the drive that holds `dir`, Puddle's data folder. Unset, the disk
    /// check says it has nothing to measure.
    /// Checks the folders above `dir`, Puddle's data folder (Linux and macOS). Unset, that check says it has
    /// nothing to read.
    #[must_use]
    pub fn with_data_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.data_dir = Some(dir.into());
        self
    }

    /// Asks the SSH agent on `pipe` (a pipe name on Windows, a socket path elsewhere) instead of the
    /// default one. A blank value counts as unset.
    #[must_use]
    pub fn with_ssh_agent_pipe(mut self, pipe: Option<String>) -> Self {
        self.ssh_agent_pipe = pipe;
        self
    }

    /// Looks for the sealed vault key in `config_home` and for `systemd-creds` in the folders of
    /// `path`, instead of the user's config folder and the process's `PATH` (tests).
    #[must_use]
    pub fn with_sealed_key_search(
        mut self,
        config_home: impl Into<PathBuf>,
        path: impl Into<std::ffi::OsString>,
    ) -> Self {
        self.sealed_key_search = (Some(config_home.into()), Some(path.into()));
        self
    }

    /// Looks for the Azure CLI in the folders of `path` (a `PATH` value) instead of the process's
    /// own. Running the real `az` can wait on the network (it checks for updates and sends
    /// telemetry), which a test of the check should not depend on.
    #[must_use]
    pub fn with_az_search_path(mut self, path: Option<std::ffi::OsString>) -> Self {
        self.az_search_path = path;
        self
    }

    /// The folder failed test boots keep their logs in, if [`Self::with_keep_boot_logs`] set one.
    #[must_use]
    pub fn keep_boot_logs_dir(&self) -> Option<&Path> {
        self.keep_boot_logs.as_deref()
    }

    /// Probes the installed runtime: `runtime/` next to `exe` (normally
    /// [`std::env::current_exe`]), the version this build was made for, and the developer
    /// override from the environment.
    ///
    /// # Errors
    ///
    /// [`RuntimeError::NoExeDir`] if `exe` has no parent folder.
    pub fn installed(exe: &Path) -> Result<Self, RuntimeError> {
        let dir = exe.parent().ok_or_else(|| RuntimeError::NoExeDir {
            path: exe.to_path_buf(),
        })?;
        Ok(Self::new(
            dir.join(RUNTIME_DIR_NAME),
            RuntimeVersion::built_for(),
            DevOverride::from_env(),
        ))
    }

    /// The layout the runtime check opens: the runtime folder and the msb home the host would
    /// use, Puddle's data folder (the one set with [`Self::with_data_dir`], else the user's).
    /// `--version` doesn't touch the home and the test boot brings its own.
    fn layout(&self) -> Result<RuntimeLayout, RuntimeError> {
        let data = match &self.data_dir {
            Some(dir) => dir.clone(),
            None => puddle_fs::data_dir().map_err(|e| RuntimeError::NoDataDir {
                reason: e.to_string(),
            })?,
        };
        RuntimeLayout::new(self.runtime_dir.clone(), data.join("msb"))
    }
}

impl Probe for SystemProbe {
    fn os(&self) -> Os {
        Os::current()
    }

    fn arch(&self) -> String {
        std::env::consts::ARCH.to_owned()
    }

    fn hypervisor(&self) -> HypervisorFacts {
        sys::hypervisor()
    }

    fn code_integrity(&self) -> Option<CodeIntegrityFacts> {
        sys::code_integrity()
    }

    fn runtime(&self) -> RuntimeFacts {
        let msb = self.runtime_dir.join(puddle_runtime::MSB_FILE_NAME);
        let facts = |state| RuntimeFacts {
            dir: self.runtime_dir.clone(),
            msb: msb.clone(),
            expected: self.expected.to_string(),
            state,
        };
        let layout = match self.layout() {
            Ok(layout) => layout,
            Err(e) => return facts(RuntimeState::Unusable(e)),
        };
        if layout.required_files().iter().all(|f| f.is_file())
            && let Err(e) = sys::file_access(&msb)
            && e.kind() == std::io::ErrorKind::PermissionDenied
        {
            return facts(RuntimeState::AccessDenied(AccessDenied {
                path: msb.clone(),
                detail: e.to_string(),
            }));
        }
        match BundledRuntime::open_with(layout, &self.expected, &self.digests, self.dev) {
            Ok(rt) => {
                let files_overridden =
                    matches!(rt.digest_status(), DigestStatus::Overridden { .. });
                let state = match rt.status() {
                    VersionStatus::Overridden { found } => RuntimeState::Ready {
                        version: found.clone().unwrap_or_else(|| "(no version)".into()),
                        overridden: true,
                    },
                    VersionStatus::Exact => RuntimeState::Ready {
                        version: self.expected.to_string(),
                        overridden: files_overridden,
                    },
                };
                // Set once: `runtime` runs once per diagnosis.
                let _first = self.opened.set(rt);
                facts(state)
            }
            Err(e) => facts(RuntimeState::Unusable(e)),
        }
    }

    fn launch(&self, limit: Duration) -> ProcessOutcome {
        match self.opened.get() {
            Some(rt) => launch::run(rt.command().arg("--version"), limit),
            None => ProcessOutcome::SpawnFailed {
                os_error: None,
                detail: "the runtime wasn't opened".into(),
            },
        }
    }

    fn boot(&self, limit: Duration) -> BootFacts {
        boot::test_boot(
            &self.runtime_dir,
            std::env::consts::ARCH,
            limit,
            self.keep_boot_logs.as_deref(),
        )
    }

    fn job(&self) -> Option<JobFacts> {
        sys::job()
    }

    fn global_secure_access(&self) -> Option<GsaFacts> {
        sys::global_secure_access()
    }

    fn disk_space(&self) -> DiskFacts {
        let Some(dir) = &self.data_dir else {
            return DiskFacts::NoFolder;
        };
        match puddle_fs::free_bytes(dir) {
            Ok(free_bytes) => DiskFacts::Read {
                dir: dir.clone(),
                free_bytes,
            },
            Err(e) => DiskFacts::Unreadable {
                dir: dir.clone(),
                detail: e.to_string(),
            },
        }
    }

    fn disk_encryption(&self) -> DiskEncryptionFacts {
        sys::disk_encryption(self.data_dir.as_deref())
    }

    fn ssh_agent(&self, limit: Duration) -> SshAgentFacts {
        ssh_agent_facts(
            AgentEndpoint::for_host(self.ssh_agent_pipe.as_deref()),
            limit,
            sys::ssh_agent_service,
        )
    }

    fn az_cli(&self, limit: Duration) -> AzCliFacts {
        az_cli_in(
            self.az_search_path
                .clone()
                .or_else(|| std::env::var_os("PATH")),
            limit,
        )
    }

    fn credential_store(&self) -> CredentialStoreFacts {
        let data = self.data_dir.clone().or_else(|| puddle_fs::data_dir().ok());
        credential_store_facts(
            data.as_deref(),
            || probe_within(puddle_secrets::probe_os_store, STORE_PROBE_WAIT),
            || login_session_in(std::env::var_os("PATH"), Duration::from_secs(2)),
            |tag| {
                let (config_home, path) = &self.sealed_key_search;
                sealed_key_facts_in(tag, config_home.as_deref(), path.clone(), SEALED_KEY_LIMIT)
            },
        )
    }

    fn data_path(&self) -> Option<DataPathFacts> {
        cfg!(unix).then(|| sys::data_path(self.data_dir.as_deref()))
    }
}

/// Which store `data` records, read without writing: the operating system's store is probed with
/// `probe` and the login session read with `session` only when that store is the one in use (or
/// nothing is decided yet).
fn credential_store_facts(
    data: Option<&Path>,
    probe: impl FnOnce() -> puddle_secrets::StoreHealth,
    session: impl FnOnce() -> puddle_secrets::LoginSession,
    sealed_key: impl FnOnce(puddle_secrets::KeySourceTag) -> VaultFacts,
) -> CredentialStoreFacts {
    let record = match data {
        Some(data) => match puddle_secrets::read_record(data) {
            Ok(record) => record,
            Err(_) => return CredentialStoreFacts::Unreadable,
        },
        None => None,
    };
    if let (Some(record), Some(data)) = (&record, data)
        && record.store == puddle_secrets::StoreKind::File
    {
        return CredentialStoreFacts::File(
            match puddle_secrets::FileVault::header_source(&puddle_secrets::vault_path(data)) {
                Ok(Some(tag)) => sealed_key(tag),
                Ok(None) => VaultFacts::NoVaultYet,
                Err(_) => VaultFacts::Unreadable,
            },
        );
    }
    CredentialStoreFacts::Os {
        recorded: record.is_some(),
        health: probe(),
        session: session(),
    }
}

/// How long the doctor waits for the operating system's credential store to take the probe write.
/// A locked keyring can ask the user to unlock it and wait for the answer; `puddle secrets status`
/// gives it the same 20 seconds.
const STORE_PROBE_WAIT: Duration = Duration::from_secs(20);

/// Runs `probe` on a thread of its own and waits `wait` for it. A store that has not answered by
/// then is reported as [`puddle_secrets::StoreHealth::Locked`] (it is waiting for an unlock); the
/// thread is left to finish by itself, which ends with this short-lived process at the latest.
fn probe_within(
    probe: impl FnOnce() -> puddle_secrets::StoreHealth + Send + 'static,
    wait: Duration,
) -> puddle_secrets::StoreHealth {
    let (send, receive) = std::sync::mpsc::channel();
    // A thread that cannot start drops `send`, which reads as no answer: locked.
    let _ = std::thread::Builder::new()
        .name("store-probe".to_owned())
        .spawn(move || {
            // The doctor may have given up waiting; nobody listens then.
            let _ = send.send(probe());
        });
    receive
        .recv_timeout(wait)
        .unwrap_or(puddle_secrets::StoreHealth::Locked)
}

/// How long each `systemd-creds` call of the sealed-key check may run.
const SEALED_KEY_LIMIT: Duration = Duration::from_secs(5);

/// What the vault's header and the sealed key file say, given a header that names `tag`. A tag of a
/// key file, or a systemd tag with no sealed file in `config_home` (the service passes the key), is
/// not examined. The sealed file is opened once through `systemd-creds` found on `path`: the key
/// goes to the null device, never into this process, and an answer that is not an exit 0 is
/// classified by the same texts the service uses.
fn sealed_key_facts_in(
    tag: puddle_secrets::KeySourceTag,
    config_home: Option<&Path>,
    path: Option<std::ffi::OsString>,
    limit: Duration,
) -> VaultFacts {
    use puddle_secrets::{KeySourceTag, Unseal};
    let sealed = config_home.map(puddle_secrets::sealed_key_path);
    let (true, Some(sealed)) = (
        tag != KeySourceTag::KeyFile,
        sealed.filter(|file| file.is_file()),
    ) else {
        return VaultFacts::Header(tag);
    };
    let Some(program) = find_on_path("systemd-creds", path) else {
        return VaultFacts::Sealed {
            tag,
            key: SealedKeyFacts::Fails(Unseal::ToolMissing),
            tpm: None,
        };
    };
    let ask = |args: &[&std::ffi::OsStr]| {
        launch::run(puddle_process::command(&program).args(args), limit)
    };
    let name = format!("--name={}", puddle_secrets::sealed_key_name());
    let decrypt = launch::run_without_output(
        puddle_process::command(&program).args([
            "--user".as_ref(),
            "decrypt".as_ref(),
            name.as_ref(),
            sealed.as_os_str(),
            std::ffi::OsStr::new("-"),
        ]),
        limit,
    );
    let key = match decrypt {
        ProcessOutcome::Exited { code: Some(0), .. } => SealedKeyFacts::Opens,
        ProcessOutcome::Exited { stderr_tail, .. } => {
            let old = matches!(
                ask(&["--version".as_ref()]),
                ProcessOutcome::Exited { code: Some(0), stdout_first_line, .. }
                    if puddle_secrets::systemd_lacks_user_sealing(&stdout_first_line)
            );
            SealedKeyFacts::Fails(if old {
                Unseal::Unsupported
            } else {
                puddle_secrets::classify_unseal_failure(&stderr_tail)
            })
        }
        ProcessOutcome::TimedOut { .. } => SealedKeyFacts::Fails(Unseal::TimedOut),
        ProcessOutcome::SpawnFailed { .. } => SealedKeyFacts::Fails(Unseal::Failed),
    };
    let tpm = (tag == KeySourceTag::SystemdHost).then(|| match ask(&["has-tpm2".as_ref()]) {
        ProcessOutcome::Exited { code: Some(0), .. } => TpmFacts::Usable,
        ProcessOutcome::Exited { code: Some(16), .. } => TpmFacts::LibrariesMissing,
        ProcessOutcome::Exited { code: Some(_), .. } => TpmFacts::Unavailable,
        _ => TpmFacts::Unknown,
    });
    VaultFacts::Sealed { tag, key, tpm }
}

/// What `loginctl` says about the display session of this user, found on `PATH`.
fn login_session_in(
    path: Option<std::ffi::OsString>,
    limit: Duration,
) -> puddle_secrets::LoginSession {
    use puddle_secrets::LoginSession;
    let (Some(program), Some(uid)) = (
        find_on_path("loginctl", path),
        puddle_secrets::login_user_id(),
    ) else {
        return LoginSession::Unknown;
    };
    let ask = |args: &[&str]| match launch::run(puddle_process::command(&program).args(args), limit)
    {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            ..
        } => Some(stdout_first_line.trim().to_owned()),
        _ => None,
    };
    let Some(display) = ask(&[
        "show-user",
        &uid.to_string(),
        "--property=Display",
        "--value",
    ]) else {
        return LoginSession::Unknown;
    };
    if display.is_empty() {
        return LoginSession::NotGraphical;
    }
    if !display.chars().all(|c| c.is_ascii_alphanumeric()) {
        return LoginSession::Unknown;
    }
    match ask(&["show-session", &display, "--property=Type", "--value"]) {
        Some(kind) => LoginSession::of_type(&kind),
        None => LoginSession::Unknown,
    }
}

/// The Azure CLI facts for a given `PATH` value.
fn az_cli_in(path: Option<std::ffi::OsString>, limit: Duration) -> AzCliFacts {
    let Some(program) = find_on_path(if cfg!(windows) { "az.cmd" } else { "az" }, path) else {
        return AzCliFacts::NotFound;
    };
    az_cli_facts_at(&program, limit)
}

/// What running `program --version` says about the Azure CLI.
fn az_cli_facts_at(program: &Path, limit: Duration) -> AzCliFacts {
    az_cli_facts(launch::run(
        puddle_process::command(program).arg("--version"),
        limit,
    ))
}

fn az_cli_facts(outcome: ProcessOutcome) -> AzCliFacts {
    match outcome {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line,
            ..
        } => match azure_cli_version(&stdout_first_line) {
            Some(version) => AzCliFacts::Found { version },
            None => AzCliFacts::Unknown {
                detail: "no version in its first line".into(),
            },
        },
        ProcessOutcome::Exited { code, .. } => AzCliFacts::Unknown {
            detail: format!("`az --version` exited with {code:?}"),
        },
        ProcessOutcome::TimedOut { after } => AzCliFacts::Unknown {
            detail: format!("`az --version` took more than {} s", after.as_secs()),
        },
        ProcessOutcome::SpawnFailed { detail, .. } => AzCliFacts::Unknown { detail },
    }
}

/// The version in the first line of `az --version`: `azure-cli   2.64.0 *`.
fn azure_cli_version(first_line: &str) -> Option<String> {
    let mut words = first_line.split_whitespace();
    (words.next()? == "azure-cli").then_some(())?;
    words.next().map(str::to_owned)
}

/// The first file called `name` in the folders of `PATH`.
fn find_on_path(name: &str, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    std::env::split_paths(&path?)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// What asking the agent gave. `service` reads the OpenSSH Authentication Agent service's state,
/// called only when the endpoint is the default pipe and nothing answers there: advice about that
/// service means nothing for a pipe the user named.
fn ssh_agent_facts(
    endpoint: Option<AgentEndpoint>,
    limit: Duration,
    service: impl FnOnce() -> Option<AgentService>,
) -> SshAgentFacts {
    let Some(endpoint) = endpoint else {
        return SshAgentFacts::NoEndpoint;
    };
    facts_from_answer(
        endpoint.to_string(),
        list_identities(&endpoint, limit),
        service,
    )
}

fn facts_from_answer(
    shown: String,
    answer: Result<Vec<Identity>, AgentError>,
    service: impl FnOnce() -> Option<AgentService>,
) -> SshAgentFacts {
    match answer {
        Ok(keys) => SshAgentFacts::Keys {
            endpoint: shown,
            families: keys
                .iter()
                .map(|k| key_family(k.key_type().unwrap_or_default()).to_owned())
                .collect(),
        },
        Err(AgentError::Unreachable(e)) => SshAgentFacts::Unreachable {
            service: (shown == WINDOWS_DEFAULT_PIPE).then(service).flatten(),
            by_default: shown == WINDOWS_DEFAULT_PIPE,
            endpoint: shown,
            detail: e.to_string(),
        },
        Err(AgentError::Refused) => SshAgentFacts::Locked { endpoint: shown },
        Err(AgentError::TimedOut(limit)) => SshAgentFacts::TimedOut {
            endpoint: shown,
            limit,
        },
        Err(e @ (AgentError::Io(_) | AgentError::Protocol(_))) => SshAgentFacts::Unusable {
            endpoint: shown,
            detail: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests;
