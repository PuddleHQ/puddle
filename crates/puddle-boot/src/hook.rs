// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running the hook: after every create, start and re-adoption, behind the gate.

use std::future::Future;
use std::time::{Duration, Instant};

use puddle_compute::{ComputeError, ExecOutput, Runtime, Sandbox, SandboxSpec};
use puddle_types::SandboxName;

use crate::assets::{AGENT_SUPERVISE_GUEST, BOOT_SH_GUEST};
use crate::gate::{Gate, GateState, GatedSandbox};
use crate::plan::BootPlan;

/// The Kubernetes tools the hook looks for.
const KUBERNETES_TOOLS: [&str; 3] = ["kind", "k3d", "minikube"];

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

/// The most characters of the reason a hook gives for a file it left as it was; the guest writes
/// it, so the rest is dropped.
pub(crate) const MAX_LEFT_REASON_CHARS: usize = 300;

/// A successful hook run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BootReport {
    /// How long it took.
    pub duration: Duration,
    /// Its progress lines (`puddle-boot: ...`).
    pub stdout: String,
    /// The files the plan merged settings into: the only ones [`BootReport::left_files`] names,
    /// since the guest's root can print anything.
    pub merged: Vec<String>,
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

    /// The hook made a session user that is not root, and the image has no `sudo`, so the user
    /// cannot become root (`puddle-boot: session user cannot become root: no sudo`). `false` for a
    /// root session user and for an image with `sudo`.
    #[must_use]
    pub fn session_user_has_no_sudo(&self) -> bool {
        self.stdout
            .lines()
            .any(|line| line == "puddle-boot: session user cannot become root: no sudo")
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

    /// The Kubernetes tools the hook found on the guest's `PATH`, from `kind`, `k3d` and
    /// `minikube` (`puddle-boot: kubernetes tools kind k3d`). Empty when there are none.
    #[must_use]
    pub fn kubernetes_tools(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .find_map(|line| line.strip_prefix("puddle-boot: kubernetes tools "))
            .map(|tools| {
                tools
                    .split_whitespace()
                    .filter(|tool| KUBERNETES_TOOLS.contains(tool))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The merged files the hook could not parse and so left as they were: each with the reason
    /// it gave (`<path> left as it is: <reason>`). The guest writes these lines, so only a file of
    /// [`BootReport::merged`] counts, once, and its reason is cut to 300 characters with every
    /// control character replaced.
    #[must_use]
    pub fn left_files(&self) -> Vec<(&str, String)> {
        let mut left: Vec<(&str, String)> = Vec::new();
        let said = self
            .stdout
            .lines()
            .filter_map(|line| line.strip_prefix("puddle-boot: "))
            .filter_map(|line| line.split_once(" left as it is: "));
        for (path, why) in said {
            if self.merged.iter().any(|m| m == path) && !left.iter().any(|(p, _)| *p == path) {
                let why = why
                    .chars()
                    .take(MAX_LEFT_REASON_CHARS)
                    .map(|c| if c.is_control() { '?' } else { c })
                    .collect();
                left.push((path, why));
            }
        }
        left
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
    /// The [`Keeper`] did not take the sandbox over (Puddle is shutting down); Puddle stopped it
    /// before the hook ran.
    #[error("workspace {sandbox:?} was not taken over and {}: {reason}", stop_text(.stop_error.as_deref()))]
    NotKept {
        /// The sandbox.
        sandbox: String,
        /// What the keeper said.
        reason: String,
        /// Set when stopping it failed too.
        stop_error: Option<Box<ComputeError>>,
    },
}

impl BootError {
    /// Whether the sandbox may still run after the failure: the hook or the keeper failed and
    /// stopping it failed too. Its owner should keep the handle so a later stop reaches it.
    #[must_use]
    pub fn left_running(&self) -> bool {
        matches!(
            self,
            Self::Hook {
                stop_error: Some(_),
                ..
            } | Self::NotKept {
                stop_error: Some(_),
                ..
            }
        )
    }
}

/// Who owns a sandbox from the moment the runtime has made or started it, before the boot hook
/// runs in it: Puddle's lifecycle, which trims and stops it at shutdown. The boot goes on with a
/// handle that does not own the VM, so a boot that is ended half-way (a shutdown that ends the
/// operations still running) does not end the VM by dropping the owning handle.
pub trait Keeper<S>: Sync {
    /// Takes over `sandbox`, the handle [`Runtime::create`] or [`Runtime::start`] gave, and
    /// returns the handle the boot goes on with. An error (shutting down) gives `sandbox` back
    /// with the reason; the boot stops it and fails with [`BootError::NotKept`].
    fn keep(&self, sandbox: S) -> impl Future<Output = Result<S, (S, String)>> + Send;
}

/// The [`Keeper`] of a caller that owns the sandbox through the handle it gets back.
#[derive(Debug, Clone, Copy, Default)]
pub struct CallerKeeps;

impl<S: Send> Keeper<S> for CallerKeeps {
    fn keep(&self, sandbox: S) -> impl Future<Output = Result<S, (S, String)>> + Send {
        std::future::ready(Ok(sandbox))
    }
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

    /// Runs the hook once in `sandbox`, as root, with `plan` on stdin, after any boot of it in
    /// flight has ended (the guest's scratch files are fixed names, so two runs must not
    /// overlap). Doesn't change the gate's state.
    ///
    /// # Errors
    ///
    /// The [`BootFailure`] when it doesn't return 0.
    pub async fn run<S: Sandbox>(
        &self,
        sandbox: &GatedSandbox<S>,
        plan: &BootPlan,
    ) -> Result<BootReport, BootFailure> {
        let _boot = sandbox.gate().lock_boot().await;
        self.exec_hook(sandbox.ungated(), plan).await
    }

    /// One hook run; the caller holds the boot lock.
    async fn exec_hook<S: Sandbox>(
        &self,
        sandbox: &S,
        plan: &BootPlan,
    ) -> Result<BootReport, BootFailure> {
        let started = Instant::now();
        let result = sandbox.exec(plan.exec_request(self.timeout)).await;
        classify(result).map(|stdout| BootReport {
            duration: started.elapsed(),
            stdout,
            merged: plan.merged_paths(),
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
        self.create_kept(runtime, spec, plan, gate, &CallerKeeps)
            .await
    }

    /// [`BootHook::create`], with `keeper` taking the sandbox over as soon as it exists.
    ///
    /// # Errors
    ///
    /// As [`BootHook::create`], and [`BootError::NotKept`].
    pub async fn create_kept<R: Runtime>(
        &self,
        runtime: &R,
        spec: SandboxSpec,
        plan: &BootPlan,
        gate: &Gate,
        keeper: &impl Keeper<R::Sandbox>,
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
        let mut booting = Booting::open(gate);
        let sandbox = runtime.create(spec).await?;
        booting.mark_vm_running();
        let sandbox = kept(sandbox, keeper).await?;
        self.finish(sandbox, plan, booting).await
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
        self.start_kept(runtime, name, plan, gate, &CallerKeeps)
            .await
    }

    /// [`BootHook::start`], with `keeper` taking the sandbox over as soon as it runs.
    ///
    /// # Errors
    ///
    /// As [`BootHook::start`], and [`BootError::NotKept`].
    pub async fn start_kept<R: Runtime>(
        &self,
        runtime: &R,
        name: &SandboxName,
        plan: &BootPlan,
        gate: &Gate,
        keeper: &impl Keeper<R::Sandbox>,
    ) -> Result<GatedSandbox<R::Sandbox>, BootError> {
        let _boot = gate.lock_boot().await;
        let mut booting = Booting::open(gate);
        let sandbox = runtime.start(name).await?;
        booting.mark_vm_running();
        let sandbox = kept(sandbox, keeper).await?;
        self.finish(sandbox, plan, booting).await
    }

    /// Re-adopts a running sandbox a previous Puddle left behind and runs the hook again
    /// (idempotent: it converges and starts nothing twice).
    /// The host has no caller for this: it re-runs the hook on a sandbox it kept with
    /// [`BootHook::run`]. The gate tests are the only callers.
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
        let mut booting = Booting::open(gate);
        let sandbox = runtime.get(name).await?;
        booting.mark_vm_running();
        self.finish(sandbox, plan, booting).await
    }

    async fn finish<S: Sandbox>(
        &self,
        sandbox: S,
        plan: &BootPlan,
        booting: Booting<'_>,
    ) -> Result<GatedSandbox<S>, BootError> {
        let gate = booting.gate;
        match self.exec_hook(&sandbox, plan).await {
            Ok(report) => {
                booting.settle(GateState::Ready);
                Ok(GatedSandbox::new(
                    sandbox,
                    gate.clone(),
                    self.ready_wait,
                    report,
                ))
            }
            Err(failure) => {
                booting.settle(GateState::Failed(failure.clone()));
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

/// One boot's hold on the gate: `Booting` from [`Booting::open`] until [`Booting::settle`]. A boot
/// that ends another way (a runtime error, or the caller dropping the future at an await) puts
/// the gate back, so it never stays `Booting` with nobody booting: to the state from before while
/// the runtime call has not answered, to `Down` once it has ([`Booting::mark_vm_running`]). Neither
/// overwrites a stop that closed the gate meanwhile.
struct Booting<'g> {
    gate: &'g Gate,
    /// What a boot that ends without a verdict leaves; `None` once settled.
    on_drop: Option<GateState>,
}

impl<'g> Booting<'g> {
    fn open(gate: &'g Gate) -> Self {
        let previous = gate.state();
        gate.set(GateState::Booting);
        Self {
            gate,
            on_drop: Some(previous),
        }
    }

    /// The runtime has made, started or found the VM, and the hook has not returned 0: the state
    /// from before no longer describes it (a gate left `Ready` by a VM that died unstopped would
    /// let exec in on an unhooked VM), so a boot that ends from here without a verdict leaves
    /// the gate `Down`.
    fn mark_vm_running(&mut self) {
        self.on_drop = Some(GateState::Down);
    }

    /// The boot ended with `state`.
    fn settle(mut self, state: GateState) {
        self.on_drop = None;
        self.gate.settle(state);
    }
}

impl Drop for Booting<'_> {
    fn drop(&mut self) {
        if let Some(state) = self.on_drop.take() {
            self.gate.settle(state);
        }
    }
}

/// Hands `sandbox` to `keeper` before anything runs in it; when the keeper refuses, stops it and
/// fails.
async fn kept<S: Sandbox>(sandbox: S, keeper: &impl Keeper<S>) -> Result<S, BootError> {
    match keeper.keep(sandbox).await {
        Ok(sandbox) => Ok(sandbox),
        Err((sandbox, reason)) => Err(BootError::NotKept {
            sandbox: sandbox.name().to_string(),
            reason,
            stop_error: sandbox.stop().await.err().map(Box::new),
        }),
    }
}

/// Turns the exec result into progress lines or a [`BootFailure`].
fn classify(result: Result<ExecOutput, ComputeError>) -> Result<String, BootFailure> {
    match result {
        Ok(out) if out.status.success() => Ok(out.stdout_text().into_owned()),
        Ok(out) => {
            let mut stderr = tail(out.stderr_text().trim_end());
            if out.stderr_truncated {
                stderr.push_str("\n(the rest of the output was cut)");
            }
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
mod tests;
