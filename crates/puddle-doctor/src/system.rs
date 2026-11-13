// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real machine as a [`Probe`].

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    AccessDenied, AgentService, AzCliFacts, BootFacts, CodeIntegrityFacts, DataPathFacts,
    DiskEncryptionFacts, DiskFacts, GsaFacts, HypervisorFacts, JobFacts, Os, ProcessOutcome,
    RuntimeFacts, RuntimeState, SshAgentFacts,
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
    /// Checks the folders above `dir`, Puddle's data folder (Linux). Unset, that check says it has
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
        az_cli_in(std::env::var_os("PATH"), limit)
    }

    fn data_path(&self) -> Option<DataPathFacts> {
        cfg!(target_os = "linux").then(|| sys::data_path(self.data_dir.as_deref()))
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
    az_cli_facts(launch::run(Command::new(program).arg("--version"), limit))
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
