// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{
    AsyncReadExt, AsyncWriteExt, AtomicBool, ComputeError, ExecContext, ExecOutput, ExecRequest,
    FakeRuntime, Op, Ordering, SSH_BANNER, Sandbox, SandboxName, SshStream, State, WorkspaceStatus,
    builtin, pty,
};

/// A handle to a sandbox in a [`FakeRuntime`].
#[derive(Debug)]
pub struct FakeSandbox {
    pub(super) runtime: FakeRuntime,
    pub(super) name: SandboxName,
    pub(super) boot: u64,
    pub(super) owned: bool,
    pub(super) forced: AtomicBool,
}

impl FakeSandbox {
    /// Checks that this handle's boot is the running one, for exec and SSH.
    pub(super) fn check_running(
        &self,
        state: &State,
        op: &'static str,
    ) -> Result<(), ComputeError> {
        let r = state
            .sandboxes
            .get(self.name.as_str())
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: self.name.to_string(),
            })?;
        if r.status != WorkspaceStatus::Running {
            return Err(ComputeError::InvalidState {
                sandbox: self.name.to_string(),
                op,
                status: r.status,
            });
        }
        if r.boot != self.boot {
            return Err(ComputeError::StaleHandle {
                sandbox: self.name.to_string(),
            });
        }
        Ok(())
    }

    pub(super) fn run(
        &self,
        state: &mut State,
        request: &ExecRequest,
    ) -> Result<ExecOutput, ComputeError> {
        let handlers = state.handlers.clone();
        let State {
            sandboxes, volumes, ..
        } = state;
        let record =
            sandboxes
                .get_mut(self.name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
        let mut env = record.booted_env.clone();
        env.extend(&request.env);
        let mut ctx = ExecContext {
            sandbox: &self.name,
            env,
            spec: &record.spec,
            root: &mut record.files,
            volumes,
        };
        if let Some(output) = handlers.iter().find_map(|h| h.handle(&mut ctx, request)) {
            return Ok(output);
        }
        builtin(&mut ctx, request)
    }
}

impl Sandbox for FakeSandbox {
    fn name(&self) -> &SandboxName {
        &self.name
    }

    fn owns_lifecycle(&self) -> bool {
        self.owned
    }

    fn stopped_by_force(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }

    async fn status(&self) -> Result<WorkspaceStatus, ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        state.enter(Op::Status, Some(self.name.as_str()), None)?;
        state
            .sandboxes
            .get(self.name.as_str())
            .map(|r| r.status)
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: self.name.to_string(),
            })
    }

    async fn stop(&self) -> Result<(), ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        state.enter(Op::Stop, Some(self.name.as_str()), None)?;
        let r =
            state
                .sandboxes
                .get_mut(self.name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
        if r.status.is_down() {
            return Ok(());
        }
        if r.boot != self.boot {
            return Err(ComputeError::StaleHandle {
                sandbox: self.name.to_string(),
            });
        }
        r.status = WorkspaceStatus::Stopped;
        let forced = state.force_stop.remove(self.name.as_str());
        self.forced.store(forced, Ordering::Relaxed);
        Ok(())
    }

    async fn exec(&self, request: ExecRequest) -> Result<ExecOutput, ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        let detail = std::iter::once(request.program.as_str())
            .chain(request.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        state.enter(Op::Exec, Some(self.name.as_str()), Some(detail))?;
        self.check_running(&state, "exec")?;
        self.run(&mut state, &request)
            .map(|output| output.capped(request.max_output))
    }

    async fn serve_ssh<S: SshStream>(&self, mut stream: S) -> Result<(), ComputeError> {
        self.runtime.turn().await;
        {
            let mut state = self.runtime.lock();
            state.enter(Op::ServeSsh, Some(self.name.as_str()), None)?;
            self.check_running(&state, "serve ssh for")?;
        }
        let io = |e: std::io::Error| ComputeError::Runtime {
            op: "serve ssh",
            message: e.to_string(),
        };
        if self.runtime.lock().terminals {
            return pty::serve_for(stream, self.name.as_str())
                .await
                .map_err(|message| ComputeError::Runtime {
                    op: "serve ssh",
                    message,
                });
        }
        stream.write_all(SSH_BANNER.as_bytes()).await.map_err(io)?;
        stream.flush().await.map_err(io)?;
        let mut sink = [0_u8; 4096];
        while stream.read(&mut sink).await.map_err(io)? > 0 {}
        Ok(())
    }
}

impl Drop for FakeSandbox {
    /// Like the SDK (msb 0.7.7): dropping an owning handle shuts the VM down; the record says
    /// `Stopped`.
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        let mut state = self.runtime.lock();
        if let Some(r) = state.sandboxes.get_mut(self.name.as_str())
            && r.boot == self.boot
            && r.status == WorkspaceStatus::Running
        {
            r.status = WorkspaceStatus::Stopped;
        }
    }
}
