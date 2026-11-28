// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl FakeRuntime {
    /// Makes every [`Runtime::pull_image_reporting`] report `steps`, in order, before it answers,
    /// as a pull that downloads would.
    pub fn script_pull(&self, steps: Vec<PullProgress>) {
        self.lock().pull_script = steps;
    }

    /// Makes `image` pullable with `config`.
    pub fn add_image(&self, image: &ImageRef, config: ImageConfig) {
        self.lock().images.insert(image.to_string(), config);
    }

    /// Arms `fault` for calls of `op`. Faults are checked in the order they were added; the
    /// first one that matches the call decides.
    pub fn inject(&self, op: Op, fault: Fault) {
        self.lock().faults.push((op, fault));
    }

    /// Disarms every fault.
    pub fn clear_faults(&self) {
        self.lock().faults.clear();
    }

    /// Every call so far, in order (including failed ones).
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Makes every call wait once (its future is pending one time) before it answers, the way a
    /// real runtime's calls wait for msb. A test can then drop the caller at each of its awaits.
    pub fn wait_in_every_call(&self, on: bool) {
        self.lock().waits = on;
    }

    /// The wait [`FakeRuntime::wait_in_every_call`] turns on.
    pub(super) async fn turn(&self) {
        let waits = self.lock().waits;
        if waits {
            WaitOnce(false).await;
        }
    }

    /// Makes every sandbox's `serve_ssh` an SSH server with a pseudo-terminal stand-in (see
    /// [`pty`]) instead of the banner-only fake.
    pub fn serve_terminals(&self) {
        self.lock().terminals = true;
    }

    /// Adds a command handler, tried before earlier handlers and before the built-ins.
    pub fn on_exec(&self, handler: impl ExecHandler) {
        self.lock().handlers.insert(0, Arc::new(handler));
    }

    /// The vsock routes `name` was made with, so a test can speak to the host as its guest would.
    /// Empty when there is no such sandbox.
    #[must_use]
    pub fn routes(&self, name: &SandboxName) -> Vec<crate::VsockRoute> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.routes.clone())
            .unwrap_or_default()
    }

    /// The owned disks `name` was made with. Empty when there is no such sandbox.
    #[must_use]
    pub fn owned_disks(&self, name: &SandboxName) -> Vec<crate::OwnedDisk> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.owned_disks.clone())
            .unwrap_or_default()
    }

    /// The root disk size `name` was made with: `None` when there is no such sandbox or its spec
    /// named none (the runtime's default).
    #[must_use]
    pub fn root_disk_size(&self, name: &SandboxName) -> Option<DiskSize> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .and_then(|r| r.spec.root_disk)
    }

    /// The named volumes `name` was made with. Empty when there is no such sandbox.
    #[must_use]
    pub fn volume_mounts(&self, name: &SandboxName) -> Vec<crate::VolumeMount> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.volumes.clone())
            .unwrap_or_default()
    }

    /// Moves every named volume of the sandbox `name` to `guest`, as in a sandbox an earlier
    /// version made with another folder. Returns whether there is such a sandbox.
    #[must_use = "false means there is no such sandbox"]
    pub fn move_volume_mounts(&self, name: &SandboxName, guest: &GuestPath) -> bool {
        let mut state = self.lock();
        let Some(record) = state.sandboxes.get_mut(name.as_str()) else {
            return false;
        };
        for volume in &mut record.spec.volumes {
            volume.guest = guest.clone();
        }
        true
    }

    /// Simulates the VM dying: a running sandbox becomes `Crashed`. Returns whether it was
    /// running.
    #[must_use = "false means the sandbox wasn't running"]
    pub fn crash(&self, name: &SandboxName) -> bool {
        let mut state = self.lock();
        match state.sandboxes.get_mut(name.as_str()) {
            Some(r) if r.status == WorkspaceStatus::Running => {
                r.status = WorkspaceStatus::Crashed;
                true
            }
            _ => false,
        }
    }

    /// Holds an existing sandbox in `status`, for the states the fake never enters by itself: a
    /// VM that is still booting (`Starting`), stopping (`Draining`) or `Paused`. The VM keeps its
    /// volumes, so it holds them like a running one; exec and `get` refuse until it runs again
    /// ([`FakeRuntime::outlive_owner`]). Returns whether the sandbox exists.
    #[must_use = "false means there is no such sandbox"]
    pub fn set_status(&self, name: &SandboxName, status: WorkspaceStatus) -> bool {
        let mut state = self.lock();
        match state.sandboxes.get_mut(name.as_str()) {
            Some(r) => {
                r.status = status;
                true
            }
            None => false,
        }
    }

    /// Simulates a VM that outlives its dead owner: the sandbox runs, whatever its handle's
    /// drop did (dropping a handle stops the fake VM, as in the SDK, but a killed process never
    /// gets to drop it). Returns whether the sandbox exists.
    #[must_use = "false means there is no such sandbox"]
    pub fn outlive_owner(&self, name: &SandboxName) -> bool {
        let mut state = self.lock();
        match state.sandboxes.get_mut(name.as_str()) {
            Some(r) => {
                r.status = WorkspaceStatus::Running;
                true
            }
            None => false,
        }
    }

    /// Adds a sandbox Puddle didn't create (another tool's, or one without Puddle's owner label).
    /// [`Runtime::list`] shows it with `puddle_owned: false`; no other operation sees it, so a
    /// test can check through [`FakeRuntime::calls`] that nothing touched it. `name` need not be
    /// a valid [`SandboxName`].
    pub fn add_foreign_sandbox(&self, name: &str, status: WorkspaceStatus) {
        self.lock().foreign.insert(name.to_owned(), status);
    }

    /// Adds a sandbox directory without a record, as a failed create leaves it or as
    /// another tool might; `name` need not be a valid [`SandboxName`].
    pub fn add_stale_dir(&self, name: &str) {
        self.lock().stale_dirs.insert(name.to_owned());
    }

    /// Adds an empty volume under any name, including names that aren't a valid [`VolumeName`]
    /// (another tool's volume).
    pub fn add_foreign_volume(&self, name: &str, size: DiskSize) {
        self.lock().volumes.insert(
            name.to_owned(),
            VolumeRecord {
                size,
                used: None,
                files: Files::new(),
            },
        );
    }

    /// Sets what the volume `name` "takes on the host" from now on; the runtime reports it in
    /// [`VolumeInfo::used`]. A volume that does not exist is left alone.
    pub fn set_volume_used(&self, name: &VolumeName, used: DiskSize) {
        if let Some(volume) = self.lock().volumes.get_mut(name.as_str()) {
            volume.used = Some(used);
        }
    }

    /// Sets how full the root disk of sandbox `name` "is" from now on; the runtime reports it in
    /// [`SandboxInfo::root_disk`]. A sandbox that does not exist is left alone.
    pub fn set_root_disk_use(&self, name: &SandboxName, size: DiskSize, used: DiskSize) {
        if let Some(record) = self.lock().sandboxes.get_mut(name.as_str()) {
            record.root_disk = Some(RootDiskUse { size, used });
        }
    }

    /// Whether a running sandbox grows its root disk live (the default). Off, it answers
    /// [`RootDiskGrowth::NextStart`] and grows at its next boot, as msb does on Windows.
    pub fn set_live_root_grow(&self, live: bool) {
        self.lock().no_live_root_grow = !live;
    }

    /// Makes the next stop of `name` time out gracefully and end the VM by force, as msb does
    /// after 30 seconds: the stop returns `Ok` and [`Sandbox::stopped_by_force`] says `true`.
    pub fn force_next_stop(&self, name: &SandboxName) {
        self.lock().force_stop.insert(name.to_string());
    }
}
