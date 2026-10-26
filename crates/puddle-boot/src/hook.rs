// SPDX-License-Identifier: GPL-3.0-or-later
//! Running the hook: after every create, start and re-adoption, behind the gate.

use std::time::{Duration, Instant};

use puddle_compute::{ComputeError, ExecOutput, Runtime, Sandbox, SandboxSpec};
use puddle_types::SandboxName;

use crate::assets::{AGENT_SUPERVISE_GUEST, BOOT_SH_GUEST};
use crate::gate::{Gate, GateState, GatedSandbox};
use crate::plan::BootPlan;

/// The most stderr a [`BootFailure`] keeps (the end of it, where the error is).
pub const STDERR_LIMIT: usize = 8 * 1024;

/// Why the hook didn't return 0. Shown to the user as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BootFailure {
    /// The image has no `/bin/sh`.
    #[error(
        "the image has no POSIX shell at /bin/sh, which Puddle's boot hook needs; use an image that has one ({detail})"
    )]
    NoShell {
        /// What the runtime said.
        detail: String,
    },
    /// The hook ran and exited non-zero.
    #[error("boot hook exited with status {code}: {stderr}")]
    Exited {
        /// Its exit status (`-1` for a signal).
        code: i32,
        /// The end of its stderr.
        stderr: String,
    },
    /// The runtime couldn't run it (timeout, VM gone).
    #[error("boot hook could not run: {0}")]
    Exec(ComputeError),
}

/// A successful hook run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootReport {
    /// How long it took.
    pub duration: Duration,
    /// Its progress lines (`puddle-boot: ...`).
    pub stdout: String,
}

impl BootReport {
    /// The files Puddle owns that held something else when the hook ran and were put back: it
    /// said `replaced <path>` for each. A file Puddle writes for the first time is not listed, and
    /// neither is one on an image with no `cmp` (there the hook cannot tell). The text a file
    /// had before is not kept.
    #[must_use]
    pub fn replaced_files(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .filter_map(|line| line.strip_prefix("puddle-boot: replaced "))
            .collect()
    }

    /// The image's entrypoint, when it exited with a failure within a second of its start: its
    /// exit status and the end of its output (`<status>: <output>`). One that is still running,
    /// exited with 0, or died later is not reported: the hook has left by then.
    #[must_use]
    pub fn entrypoint_failure(&self) -> Option<&str> {
        self.stdout
            .lines()
            .find_map(|line| line.strip_prefix("puddle-boot: entrypoint exited with status "))
    }

    /// The git the guest has, as `(major, minor)`, when the hook found one and could read its
    /// version (`puddle-boot: git version 2.39.5`). `None` for a guest with no git yet.
    #[must_use]
    pub fn git_version(&self) -> Option<(u32, u32)> {
        let text = self
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("puddle-boot: git version "))?;
        let mut parts = text.split(|c: char| !c.is_ascii_digit());
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some((major, minor))
    }

    /// The merged files the hook could not parse and so left as they were: each with the reason
    /// it gave (`<path> left as it is: <reason>`).
    #[must_use]
    pub fn left_files(&self) -> Vec<(&str, &str)> {
        self.stdout
            .lines()
            .filter_map(|line| line.strip_prefix("puddle-boot: "))
            .filter_map(|line| line.split_once(" left as it is: "))
            .collect()
    }
}

/// Why a create, start or adopt with the hook failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BootError {
    /// The spec lacks a mount the hook needs.
    #[error("sandbox spec has no read-only mount at {guest}, which the boot hook needs")]
    MissingMount {
        /// The guest path.
        guest: String,
    },
    /// The runtime call before the hook failed.
    #[error(transparent)]
    Compute(#[from] ComputeError),
    /// The hook failed; Puddle stopped the sandbox (fail closed: no half-set-up VM runs).
    #[error("workspace {sandbox:?} failed its boot hook and {}: {failure}", stop_text(.stop_error.as_deref()))]
    Hook {
        /// The sandbox.
        sandbox: String,
        /// Why (boxed: keeps `BootError` small).
        failure: Box<BootFailure>,
        /// Set when stopping it failed too.
        stop_error: Option<Box<ComputeError>>,
    },
}

/// What [`BootError::Hook`] says became of the sandbox: stopped, or still up because the stop
/// failed too.
fn stop_text(stop_error: Option<&ComputeError>) -> String {
    match stop_error {
        None => "was stopped".to_owned(),
        Some(e) => format!("could not be stopped either ({e}), so it may still be running"),
    }
}

/// Runs the boot hook and keeps the [`Gate`] in step. One per Puddle; cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootHook {
    timeout: Duration,
    ready_wait: Duration,
}

impl Default for BootHook {
    fn default() -> Self {
        Self::new()
    }
}

impl BootHook {
    /// Default hook timeout: 60 s (a normal boot takes about 1 s).
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
    /// Default time a gated call waits for a booting sandbox: 90 s.
    pub const DEFAULT_READY_WAIT: Duration = Duration::from_secs(90);

    /// A hook with the default timeouts.
    #[must_use]
    pub fn new() -> Self {
        Self {
            timeout: Self::DEFAULT_TIMEOUT,
            ready_wait: Self::DEFAULT_READY_WAIT,
        }
    }

    /// Sets how long the hook may run.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets how long gated calls wait while the sandbox boots.
    #[must_use]
    pub fn with_ready_wait(mut self, wait: Duration) -> Self {
        self.ready_wait = wait;
        self
    }

    /// Runs the hook once in `sandbox`, as root, with `plan` on stdin. Doesn't touch any gate.
    ///
    /// # Errors
    ///
    /// The [`BootFailure`] when it doesn't return 0.
    pub async fn run<S: Sandbox>(
        &self,
        sandbox: &S,
        plan: &BootPlan,
    ) -> Result<BootReport, BootFailure> {
        let started = Instant::now();
        let result = sandbox.exec(plan.exec_request(self.timeout)).await;
        classify(result).map(|stdout| BootReport {
            duration: started.elapsed(),
            stdout,
        })
    }

    /// Creates the sandbox, runs the hook, opens the gate. `spec` must mount the boot scripts
    /// (see [`crate::with_boot_mounts`]).
    ///
    /// # Errors
    ///
    /// [`BootError::MissingMount`] before anything is created; [`BootError::Compute`] from
    /// [`Runtime::create`]; [`BootError::Hook`] when the hook fails (the sandbox is stopped).
    pub async fn create<R: Runtime>(
        &self,
        runtime: &R,
        spec: SandboxSpec,
        plan: &BootPlan,
        gate: &Gate,
    ) -> Result<GatedSandbox<R::Sandbox>, BootError> {
        let merge_tool = plan.merge_tool();
        let mut needed = vec![BOOT_SH_GUEST, AGENT_SUPERVISE_GUEST];
        if let Some(agent) = plan.agent() {
            needed.push(agent.binary.as_str());
        }
        if plan.has_merged_files() {
            needed.push(merge_tool.as_str());
        }
        for guest in needed {
            if !spec.file_mounts.iter().any(|m| m.guest.as_str() == guest) {
                return Err(BootError::MissingMount {
                    guest: guest.to_owned(),
                });
            }
        }
        let _boot = gate.lock_boot().await;
        let previous = open_boot(gate);
        match runtime.create(spec).await {
            Ok(sandbox) => self.finish(sandbox, plan, gate).await,
            Err(e) => {
                gate.set(previous);
                Err(e.into())
            }
        }
    }

    /// Starts a stopped or crashed sandbox, runs the hook, opens the gate (the guest resets its
    /// sysctls on every boot).
    ///
    /// # Errors
    ///
    /// [`BootError::Compute`] from [`Runtime::start`]; [`BootError::Hook`].
    pub async fn start<R: Runtime>(
        &self,
        runtime: &R,
        name: &SandboxName,
        plan: &BootPlan,
        gate: &Gate,
    ) -> Result<GatedSandbox<R::Sandbox>, BootError> {
        let _boot = gate.lock_boot().await;
        let previous = open_boot(gate);
        match runtime.start(name).await {
            Ok(sandbox) => self.finish(sandbox, plan, gate).await,
            Err(e) => {
                gate.set(previous);
                Err(e.into())
            }
        }
    }

    /// Re-adopts a running sandbox a previous Puddle left behind and runs the hook again
    /// (idempotent: it converges and starts nothing twice).
    ///
    /// # Errors
    ///
    /// [`BootError::Compute`] from [`Runtime::get`]; [`BootError::Hook`].
    pub async fn adopt<R: Runtime>(
        &self,
        runtime: &R,
        name: &SandboxName,
        plan: &BootPlan,
        gate: &Gate,
    ) -> Result<GatedSandbox<R::Sandbox>, BootError> {
        let _boot = gate.lock_boot().await;
        let previous = open_boot(gate);
        match runtime.get(name).await {
            Ok(sandbox) => self.finish(sandbox, plan, gate).await,
            Err(e) => {
                gate.set(previous);
                Err(e.into())
            }
        }
    }

    async fn finish<S: Sandbox>(
        &self,
        sandbox: S,
        plan: &BootPlan,
        gate: &Gate,
    ) -> Result<GatedSandbox<S>, BootError> {
        match self.run(&sandbox, plan).await {
            Ok(report) => {
                gate.set(GateState::Ready);
                Ok(GatedSandbox::new(
                    sandbox,
                    gate.clone(),
                    self.ready_wait,
                    report,
                ))
            }
            Err(failure) => {
                gate.set(GateState::Failed(failure.clone()));
                let stop_error = sandbox.stop().await.err().map(Box::new);
                Err(BootError::Hook {
                    sandbox: sandbox.name().to_string(),
                    failure: Box::new(failure),
                    stop_error,
                })
            }
        }
    }
}

/// Sets the gate to `Booting` and returns what it was.
fn open_boot(gate: &Gate) -> GateState {
    let previous = gate.state();
    gate.set(GateState::Booting);
    previous
}

/// Turns the exec result into progress lines or a [`BootFailure`].
fn classify(result: Result<ExecOutput, ComputeError>) -> Result<String, BootFailure> {
    match result {
        Ok(out) if out.status.success() => Ok(out.stdout_text().into_owned()),
        Ok(out) => {
            let stderr = tail(out.stderr_text().trim_end());
            // `sh` itself missing: 127 with no word from the hook and no mention of the script
            // (a missing script mount also gives 127 under bash, but names the script).
            if out.status.code == 127
                && !stderr.contains("puddle-boot:")
                && !stderr.contains(BOOT_SH_GUEST)
            {
                Err(BootFailure::NoShell { detail: stderr })
            } else {
                Err(BootFailure::Exited {
                    code: out.status.code,
                    stderr,
                })
            }
        }
        Err(ComputeError::Runtime { op, message }) if spawn_not_found(&message) => {
            Err(BootFailure::NoShell {
                detail: format!("runtime failed to {op}: {message}"),
            })
        }
        Err(e) => Err(BootFailure::Exec(e)),
    }
}

/// Whether a runtime error says the program wasn't there (wording of the msb guest agent's
/// spawn error; the VM test pins it).
fn spawn_not_found(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("no such file") || m.contains("not found")
}

/// The last [`STDERR_LIMIT`] bytes of `s`, cut at a character boundary.
fn tail(s: &str) -> String {
    if s.len() <= STDERR_LIMIT {
        return s.to_owned();
    }
    let mut start = s.len() - STDERR_LIMIT;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    format!("...{}", s.get(start..).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_lists_the_files_the_hook_put_back() {
        let report = BootReport {
            duration: Duration::ZERO,
            stdout: "puddle-boot: sysctls set\npuddle-boot: replaced /etc/a b\n\
                     puddle-boot: replaced /etc/c\npuddle-boot: ready\n"
                .into(),
        };
        assert_eq!(report.replaced_files(), ["/etc/a b", "/etc/c"]);
        assert_eq!(BootReport::default().replaced_files(), [] as [&str; 0]);
    }

    #[test]
    fn the_report_names_an_entrypoint_that_failed_at_once() {
        let said = |stdout: &str| BootReport {
            duration: Duration::ZERO,
            stdout: stdout.into(),
        };
        assert_eq!(
            said("puddle-boot: entrypoint chained: x\npuddle-boot: entrypoint exited with status 127: x: not found\n")
                .entrypoint_failure(),
            Some("127: x: not found")
        );
        assert_eq!(
            said("puddle-boot: entrypoint chained: x\n").entrypoint_failure(),
            None
        );
        assert_eq!(BootReport::default().entrypoint_failure(), None);
    }

    #[test]
    fn the_report_names_the_guests_git_version() {
        let said = |stdout: &str| BootReport {
            duration: Duration::ZERO,
            stdout: stdout.into(),
        };
        assert_eq!(
            said("puddle-boot: git version 2.39.5\n").git_version(),
            Some((2, 39))
        );
        assert_eq!(
            said("puddle-boot: git version 2.34.1.windows.1\n").git_version(),
            Some((2, 34))
        );
        assert_eq!(
            said("puddle-boot: git version 2.39.3 (Apple Git-146)\n").git_version(),
            Some((2, 39))
        );
        assert_eq!(
            said("puddle-boot: git version unknown\n").git_version(),
            None
        );
        assert_eq!(said("puddle-boot: ready\n").git_version(), None);
    }

    #[test]
    fn classify_sorts_outcomes() {
        assert_eq!(
            classify(Ok(ExecOutput::new(0, "puddle-boot: ready\n", ""))),
            Ok("puddle-boot: ready\n".into())
        );
        assert_eq!(
            classify(Ok(ExecOutput::new(1, "", "puddle-boot: error: x\n"))),
            Err(BootFailure::Exited {
                code: 1,
                stderr: "puddle-boot: error: x".into()
            })
        );
        assert!(matches!(
            classify(Ok(ExecOutput::new(127, "", "exec: /bin/sh: not found"))),
            Err(BootFailure::NoShell { .. })
        ));
        assert!(matches!(
            classify(Ok(ExecOutput::new(
                127,
                "",
                "bash: /puddle/boot.sh: No such file or directory"
            ))),
            Err(BootFailure::Exited { code: 127, .. })
        ));
        let spawn = ComputeError::Runtime {
            op: "exec",
            message: "spawn /bin/sh: No such file or directory (os error 2)".into(),
        };
        let err = classify(Err(spawn)).unwrap_err();
        assert!(
            err.to_string().contains("no POSIX shell at /bin/sh"),
            "{err}"
        );
        let timeout = ComputeError::ExecTimeout {
            sandbox: "a".into(),
            program: "/bin/sh".into(),
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            classify(Err(timeout.clone())),
            Err(BootFailure::Exec(timeout))
        );
    }

    #[test]
    fn stderr_keeps_its_end() {
        let long = format!("{}é{}", "a".repeat(STDERR_LIMIT), "the error");
        let t = tail(&long);
        assert!(t.starts_with("..."));
        assert!(t.ends_with("the error"));
        assert!(t.len() <= STDERR_LIMIT + 3);
        assert_eq!(tail("short"), "short");
    }

    #[test]
    fn hook_settings() {
        let h = BootHook::default()
            .with_timeout(Duration::from_secs(1))
            .with_ready_wait(Duration::from_secs(2));
        assert_eq!(h.timeout, Duration::from_secs(1));
        assert_eq!(h.ready_wait, Duration::from_secs(2));
        assert_eq!(BootHook::new().timeout, BootHook::DEFAULT_TIMEOUT);
    }
}
