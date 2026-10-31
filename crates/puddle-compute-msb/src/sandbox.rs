// SPDX-License-Identifier: GPL-3.0-or-later
//! [`MsbSandbox`]: one sandbox handle, bound to one boot.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use microsandbox::sandbox::SandboxHandle;
use microsandbox::{MicrosandboxError, Sandbox as SdkSandbox, SshServer};
use puddle_compute::{ComputeError, ExecOutput, ExecRequest, Sandbox, SshStream};
use puddle_types::{SandboxName, WorkspaceStatus};
use tokio::sync::OnceCell;

use crate::error::{map, runtime};
use crate::runtime::{MsbRuntime, status};

/// How long [`Sandbox::stop`] waits for a graceful shutdown before killing the VM.
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the guest's processes get to exit after their SIGTERM before a stop goes ahead and the
/// runtime ends what is left (`docker stop` gives a container the same).
pub(crate) const TERM_GRACE: Duration = Duration::from_secs(10);

/// Slack on top of [`TERM_GRACE`] for the exec that gives the SIGTERM: a guest that does not
/// answer at all costs the stop no more than the two together.
const TERM_SLACK: Duration = Duration::from_secs(2);

/// `guest_term.sh`: gives every process of the guest one SIGTERM and waits for them to exit.
const GUEST_TERM_SH: &str = include_str!("guest_term.sh");

/// The exec [`Sandbox::stop`] runs before the stop itself (why: the script's header).
fn term_request() -> ExecRequest {
    ExecRequest::new(
        "sh",
        [
            "-c".to_owned(),
            GUEST_TERM_SH.to_owned(),
            "puddle-term".to_owned(),
            TERM_GRACE.as_secs().to_string(),
        ],
    )
    .as_user("root")
    .with_timeout(TERM_GRACE + TERM_SLACK)
}

/// Slack on top of an exec's own timeout before the adapter gives up waiting for the SDK.
const EXEC_GRACE: Duration = Duration::from_secs(30);

/// Which boot a handle belongs to: the VM process id msb recorded for it.
pub(crate) fn boot_id(record: &SandboxHandle) -> Option<i32> {
    record.local().and_then(|l| l.pid)
}

/// A handle to one msb sandbox. Handles from [`puddle_compute::Runtime::create`] and
/// [`puddle_compute::Runtime::start`] own the VM (dropping them kills it); handles from
/// [`puddle_compute::Runtime::get`] don't. A handle belongs to the boot it was made for: after a
/// restart, exec, SSH and stop through it fail with [`ComputeError::StaleHandle`].
pub struct MsbSandbox {
    runtime: MsbRuntime,
    name: SandboxName,
    sdk: SdkSandbox,
    boot: Option<i32>,
    /// Set when a stop ended the VM by force.
    forced: AtomicBool,
    /// The SSH server every connection of this boot is served with, set up by the first one:
    /// the SDK's server is made to be reused. Setting it up loads the sandbox's host key and
    /// creates it when missing, which the SDK does not do atomically, so a server per
    /// connection let the first connections of a new sandbox race to create the key and all but
    /// one fail with "File exists". Boxed: the SDK server is large and the handle lives in
    /// many futures.
    ssh: OnceCell<Box<SshServer>>,
}

impl std::fmt::Debug for MsbSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MsbSandbox")
            .field("name", &self.name)
            .field("boot", &self.boot)
            .field("owns_lifecycle", &self.sdk.owns_lifecycle())
            .finish_non_exhaustive()
    }
}

/// Whether a failed graceful stop means the VM did not answer, so killing it is the way to
/// finish the stop: the shutdown took longer than the timeout, or the request never reached the
/// guest (a VM that is hung or frozen does not complete the handshake with msb's relay).
fn stop_went_unanswered(error: &MicrosandboxError) -> bool {
    use microsandbox::agent::AgentClientError;
    matches!(
        error,
        MicrosandboxError::StopTimeout { .. }
            | MicrosandboxError::SandboxStopTimedOut { .. }
            | MicrosandboxError::AgentClient(
                AgentClientError::Handshake(_) | AgentClientError::Connect { .. }
            )
    )
}

/// An exec that couldn't start, as the shell would report it: 127 for a missing program, 126 for
/// one that can't run. Anything else stays an error.
fn spawn_failure(failure: &microsandbox::protocol::exec::ExecFailed) -> Option<ExecOutput> {
    use microsandbox::protocol::exec::ExecFailureKind;
    let code = match failure.kind {
        ExecFailureKind::NotFound => 127,
        ExecFailureKind::PermissionDenied | ExecFailureKind::NotExecutable => 126,
        _ => return None,
    };
    Some(ExecOutput::new(
        code,
        Vec::new(),
        format!("{}\n", failure.message).into_bytes(),
    ))
}

impl MsbSandbox {
    pub(crate) fn new(
        runtime: MsbRuntime,
        name: SandboxName,
        sdk: SdkSandbox,
        boot: Option<i32>,
    ) -> Self {
        Self {
            runtime,
            name,
            sdk,
            boot,
            forced: AtomicBool::new(false),
            ssh: OnceCell::new(),
        }
    }

    /// The current record, checked to be this handle's boot and running.
    async fn this_boot(&self, op: &'static str) -> Result<SandboxHandle, ComputeError> {
        let record = self
            .runtime
            .record(self.name.as_str())
            .await?
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: self.name.to_string(),
            })?;
        let current = status(record.status_snapshot());
        if current != WorkspaceStatus::Running {
            return Err(ComputeError::InvalidState {
                sandbox: self.name.to_string(),
                op,
                status: current,
            });
        }
        if boot_id(&record) != self.boot {
            return Err(ComputeError::StaleHandle {
                sandbox: self.name.to_string(),
            });
        }
        Ok(record)
    }
}

impl MsbSandbox {
    /// Gives the guest's processes their SIGTERM and up to [`TERM_GRACE`] to exit. Best effort:
    /// whatever goes wrong here, the stop goes ahead and the runtime ends what is left.
    async fn term_guest_processes(&self) {
        let ran = tokio::time::timeout(TERM_GRACE + TERM_SLACK, self.exec(term_request())).await;
        if let Some(problem) = term_problem(ran.ok()) {
            tracing::warn!(sandbox = %self.name, "{problem}; the stop goes ahead");
        }
    }
}

/// What went wrong with the SIGTERM step, from its result (`None` when the guest did not answer in
/// time); `None` when every process exited within the grace period.
fn term_problem(ran: Option<Result<ExecOutput, ComputeError>>) -> Option<String> {
    match ran {
        Some(Ok(out)) if out.status.success() => None,
        Some(Ok(out)) => Some(format!(
            "guest processes still ran after their SIGTERM and the grace period ({})",
            out.stdout_text().trim()
        )),
        Some(Err(e)) => Some(format!(
            "could not give the guest's processes their SIGTERM: {e}"
        )),
        None => Some("the guest did not answer the SIGTERM step in time".to_owned()),
    }
}

// Every method boxes its future: the SDK's futures are large, and callers nest them (the
// contract suite overflowed a test thread's stack with them inline).
impl Sandbox for MsbSandbox {
    fn stopped_by_force(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }

    fn name(&self) -> &SandboxName {
        &self.name
    }

    fn owns_lifecycle(&self) -> bool {
        self.sdk.owns_lifecycle()
    }

    fn status(&self) -> impl Future<Output = Result<WorkspaceStatus, ComputeError>> + Send {
        Box::pin(async move {
            let record = self
                .runtime
                .record(self.name.as_str())
                .await?
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
            Ok(status(record.status_snapshot()))
        })
    }

    fn stop(&self) -> impl Future<Output = Result<(), ComputeError>> + Send {
        Box::pin(async move {
            let record = self
                .runtime
                .record(self.name.as_str())
                .await?
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
            if status(record.status_snapshot()).is_down() {
                return Ok(());
            }
            if boot_id(&record) != self.boot {
                return Err(ComputeError::StaleHandle {
                    sandbox: self.name.to_string(),
                });
            }
            self.term_guest_processes().await;
            match self
                .runtime
                .sdk(self.sdk.stop_with_timeout(STOP_TIMEOUT))
                .await
            {
                Ok(()) => Ok(()),
                Err(e) if stop_went_unanswered(&e) => {
                    tracing::warn!(sandbox = %self.name, error = %e, "graceful stop got no answer; killing the VM");
                    self.runtime
                        .sdk(self.sdk.kill())
                        .await
                        .map_err(|e| map("kill", self.name.as_str(), e))?;
                    // Down, but not shut down: the caller can ask and tell the user.
                    self.forced.store(true, Ordering::Relaxed);
                    Ok(())
                }
                Err(e) => Err(map("stop", self.name.as_str(), e)),
            }
        })
    }

    fn exec(
        &self,
        request: ExecRequest,
    ) -> impl Future<Output = Result<ExecOutput, ComputeError>> + Send {
        Box::pin(async move {
            self.this_boot("exec").await?;
            let ExecRequest {
                program,
                args,
                env,
                cwd,
                user,
                stdin,
                timeout,
            } = request;
            let env: Vec<(String, String)> = env
                .iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            let call = self.sdk.exec_with(program.clone(), move |mut e| {
                e = e.args(args).envs(env).timeout(timeout).tty(false);
                if let Some(cwd) = cwd {
                    e = e.cwd(cwd.as_str());
                }
                if let Some(user) = user {
                    e = e.user(user);
                }
                if stdin.is_empty() {
                    e.stdin_null()
                } else {
                    e.stdin_bytes(stdin)
                }
            });
            let timed_out = || ComputeError::ExecTimeout {
                sandbox: self.name.to_string(),
                program: program.clone(),
                timeout,
            };
            let Ok(result) =
                tokio::time::timeout(timeout + EXEC_GRACE, self.runtime.sdk(call)).await
            else {
                return Err(timed_out());
            };
            match result {
                Ok(output) => Ok(ExecOutput::new(
                    output.status().code,
                    output.stdout_bytes().to_vec(),
                    output.stderr_bytes().to_vec(),
                )),
                Err(MicrosandboxError::ExecTimeout(_)) => Err(timed_out()),
                Err(MicrosandboxError::ExecFailed(failure)) => {
                    spawn_failure(&failure).ok_or_else(|| runtime("exec", &failure.message))
                }
                Err(e) => Err(map("exec", self.name.as_str(), e)),
            }
        })
    }

    fn serve_ssh<S: SshStream>(
        &self,
        stream: S,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        Box::pin(async move {
            self.this_boot("serve ssh for").await?;
            let server = self
                .ssh
                .get_or_try_init(|| async {
                    let ssh = self.runtime.config().ssh.clone();
                    let ops = self.sdk.ssh();
                    self.runtime
                        .sdk(ops.server_with(move |mut o| {
                            for key in ssh.authorized_keys {
                                o = o.authorized_key(key);
                            }
                            match ssh.inactivity_timeout {
                                Some(t) => o.inactivity_timeout(t),
                                None => o.disable_inactivity_timeout(),
                            }
                        }))
                        .await
                        .map(Box::new)
                })
                .await
                .map_err(|e| runtime("serve ssh", &e))?;
            self.runtime
                .sdk(server.serve_connection(stream))
                .await
                .map_err(|e| runtime("serve ssh", &e))
        })
    }
}

#[cfg(test)]
mod tests {
    use microsandbox::protocol::exec::{ExecFailed, ExecFailureKind};

    use super::*;

    #[test]
    fn the_sigterm_step_runs_the_script_as_root_with_the_grace_period() {
        let request = term_request();
        assert_eq!(request.program, "sh");
        assert_eq!(
            request.args,
            ["-c", GUEST_TERM_SH, "puddle-term", "10"].map(str::to_owned)
        );
        assert_eq!(request.user.as_deref(), Some("root"));
        assert_eq!(request.timeout, Duration::from_secs(12));
    }

    /// The script only ever runs in a guest (it signals every process it can see), so here it is
    /// only parsed.
    #[cfg(unix)]
    #[test]
    fn the_sigterm_script_parses() {
        let out = std::process::Command::new("sh")
            .args(["-n", "-c", GUEST_TERM_SH])
            .output()
            .expect("run sh -n");
        assert!(out.status.success(), "{out:?}");
    }

    #[test]
    fn the_sigterm_step_says_what_went_wrong() {
        assert_eq!(term_problem(Some(Ok(ExecOutput::new(0, "", "")))), None);
        assert_eq!(
            term_problem(Some(Ok(ExecOutput::new(1, "left: 41 dockerd\n", "")))).as_deref(),
            Some(
                "guest processes still ran after their SIGTERM and the grace period (left: 41 dockerd)"
            )
        );
        let gone = ComputeError::StaleHandle {
            sandbox: "w".to_owned(),
        };
        assert_eq!(
            term_problem(Some(Err(gone.clone()))),
            Some(format!(
                "could not give the guest's processes their SIGTERM: {gone}"
            ))
        );
        assert_eq!(
            term_problem(None).as_deref(),
            Some("the guest did not answer the SIGTERM step in time")
        );
    }

    fn failed(kind: ExecFailureKind) -> ExecFailed {
        ExecFailed {
            kind,
            errno: Some(2),
            errno_name: Some("ENOENT".into()),
            message: "nope: not found".into(),
            stage: Some("execvp".into()),
        }
    }

    #[test]
    fn spawn_failures_read_like_a_shell() {
        let out = spawn_failure(&failed(ExecFailureKind::NotFound)).unwrap();
        assert_eq!(out.status.code, 127);
        assert_eq!(out.stderr_text(), "nope: not found\n");
        for kind in [
            ExecFailureKind::PermissionDenied,
            ExecFailureKind::NotExecutable,
        ] {
            assert_eq!(spawn_failure(&failed(kind)).unwrap().status.code, 126);
        }
        for kind in [
            ExecFailureKind::BadCwd,
            ExecFailureKind::BadArgs,
            ExecFailureKind::ResourceLimit,
            ExecFailureKind::UserSetupFailed,
        ] {
            assert!(spawn_failure(&failed(kind)).is_none());
        }
    }

    #[test]
    fn a_stop_the_vm_did_not_answer_is_ended_by_a_kill_and_any_other_failure_is_not() {
        use microsandbox::agent::AgentClientError;
        let unanswered = [
            MicrosandboxError::StopTimeout {
                name: "box".into(),
                identity: "box#1".into(),
                timeout: Duration::from_secs(30),
            },
            MicrosandboxError::SandboxStopTimedOut {
                name: "box".into(),
                timeout: Duration::from_secs(30),
            },
            AgentClientError::Handshake("read id range: timed out before relay sent bytes".into())
                .into(),
            AgentClientError::Connect {
                // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
                path: "/no/relay.sock".into(),
                source: std::io::Error::other("refused"),
            }
            .into(),
        ];
        for e in &unanswered {
            assert!(stop_went_unanswered(e), "{e}");
        }
        for e in [
            MicrosandboxError::SandboxNotFound("box".into()),
            MicrosandboxError::Runtime("vmm gone".into()),
            AgentClientError::SandboxNotFound("box".into()).into(),
        ] {
            assert!(!stop_went_unanswered(&e), "{e}");
        }
    }
}
