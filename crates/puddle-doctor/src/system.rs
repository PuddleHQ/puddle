// SPDX-License-Identifier: GPL-3.0-or-later
//! The real machine as a [`Probe`].

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_runtime::{
    BundledRuntime, DevOverride, RUNTIME_DIR_NAME, RuntimeError, RuntimeLayout, RuntimeVersion,
    VersionStatus,
};

use puddle_ssh_agent::{
    AgentEndpoint, AgentError, Identity, WINDOWS_DEFAULT_PIPE, key_family, list_identities,
};

use crate::diagnose::Probe;
use crate::facts::{
    AccessDenied, AgentService, BootFacts, CodeIntegrityFacts, DataPathFacts, DiskEncryptionFacts,
    DiskFacts, GsaFacts, HypervisorFacts, JobFacts, Os, ProcessOutcome, RuntimeFacts, RuntimeState,
    SshAgentFacts,
};
use crate::{boot, launch, sys};

/// Probes this machine and the bundled runtime in one folder.
#[derive(Debug)]
pub struct SystemProbe {
    runtime_dir: PathBuf,
    expected: RuntimeVersion,
    dev: DevOverride,
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
            keep_boot_logs: None,
            data_dir: None,
            ssh_agent_pipe: None,
            opened: OnceCell::new(),
        }
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

    #[expect(
        clippy::disallowed_methods,
        reason = "the home is never read or written here; it only has to be an absolute folder"
    )]
    fn layout(&self) -> Result<RuntimeLayout, RuntimeError> {
        // The home is unused here: `--version` doesn't touch it and the test boot brings its own.
        RuntimeLayout::new(self.runtime_dir.clone(), std::env::temp_dir())
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
        match BundledRuntime::open(layout, &self.expected, self.dev) {
            Ok(rt) => {
                let state = match rt.status() {
                    VersionStatus::Overridden { found } => RuntimeState::Ready {
                        version: found.clone().unwrap_or_else(|| "(no version)".into()),
                        overridden: true,
                    },
                    VersionStatus::Exact => RuntimeState::Ready {
                        version: self.expected.to_string(),
                        overridden: false,
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

    fn data_path(&self) -> Option<DataPathFacts> {
        cfg!(target_os = "linux").then(|| sys::data_path(self.data_dir.as_deref()))
    }
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
