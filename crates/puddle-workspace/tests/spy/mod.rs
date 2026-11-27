// SPDX-License-Identifier: AGPL-3.0-or-later
//! A runtime that wraps the fake and shows a test two things the fake cannot: what the caller's
//! state is while a call runs, and a call that never answers (a request that is dropped).
//!
//! The fake answers every call at once, so a future built on it cannot be dropped half way.
//! Here a hook sees each runtime call before the fake does and says whether it goes through.

use std::sync::Arc;

use puddle_compute::fake::{FakeRuntime, Op};
use puddle_compute::{
    Capabilities, ComputeError, DiskSize, ImageConfig, RootDiskGrowth, Runtime, SandboxInfo,
    SandboxSpec, VolumeInfo, VolumeSpec,
};
use puddle_types::{CpuCount, GuestEnv, ImageRef, MemoryMib, SandboxName, VolumeName};

/// What a hook says about one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The fake answers.
    Pass,
    /// The call never answers.
    Hang,
    /// The fake does the call and the answer never arrives: a sandbox it made is held by the
    /// future that waits for it, and gone with that future ([`Runtime::create`] only).
    HangAfter,
}

type Hook = dyn Fn(Op) -> Step + Send + Sync;

/// The fake behind a hook.
pub(crate) struct Spy {
    pub(crate) fake: FakeRuntime,
    hook: Arc<Hook>,
}

impl Spy {
    pub(crate) fn new(
        fake: FakeRuntime,
        hook: impl Fn(Op) -> Step + Send + Sync + 'static,
    ) -> Self {
        Self {
            fake,
            hook: Arc::new(hook),
        }
    }

    async fn gate(&self, op: Op) {
        if (self.hook)(op) == Step::Hang {
            std::future::pending::<()>().await;
        }
    }
}

impl Runtime for Spy {
    type Sandbox = <FakeRuntime as Runtime>::Sandbox;

    async fn probe(&self) -> Result<Capabilities, ComputeError> {
        self.gate(Op::Probe).await;
        self.fake.probe().await
    }

    async fn pull_image(&self, image: &ImageRef) -> Result<ImageConfig, ComputeError> {
        self.gate(Op::PullImage).await;
        self.fake.pull_image(image).await
    }

    async fn create(&self, spec: SandboxSpec) -> Result<Self::Sandbox, ComputeError> {
        let step = (self.hook)(Op::Create);
        if step == Step::Hang {
            std::future::pending::<()>().await;
        }
        let made = self.fake.create(spec).await;
        if step == Step::HangAfter {
            std::future::pending::<()>().await;
        }
        made
    }

    async fn start(&self, name: &SandboxName) -> Result<Self::Sandbox, ComputeError> {
        self.gate(Op::Start).await;
        self.fake.start(name).await
    }

    async fn get(&self, name: &SandboxName) -> Result<Self::Sandbox, ComputeError> {
        self.gate(Op::Get).await;
        self.fake.get(name).await
    }

    async fn set_memory(&self, name: &SandboxName, memory: MemoryMib) -> Result<(), ComputeError> {
        self.gate(Op::SetMemory).await;
        self.fake.set_memory(name, memory).await
    }

    async fn set_cpus(&self, name: &SandboxName, cpus: CpuCount) -> Result<(), ComputeError> {
        self.gate(Op::SetCpus).await;
        self.fake.set_cpus(name, cpus).await
    }

    async fn set_env(&self, name: &SandboxName, env: &GuestEnv) -> Result<(), ComputeError> {
        self.gate(Op::SetEnv).await;
        self.fake.set_env(name, env).await
    }

    async fn grow_root_disk(
        &self,
        name: &SandboxName,
        size: DiskSize,
    ) -> Result<RootDiskGrowth, ComputeError> {
        self.gate(Op::GrowRootDisk).await;
        self.fake.grow_root_disk(name, size).await
    }

    async fn list(&self) -> Result<Vec<SandboxInfo>, ComputeError> {
        self.gate(Op::List).await;
        self.fake.list().await
    }

    async fn remove(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate(Op::Remove).await;
        self.fake.remove(name).await
    }

    async fn kill(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate(Op::Kill).await;
        self.fake.kill(name).await
    }

    async fn stale_dirs(&self) -> Result<Vec<String>, ComputeError> {
        self.gate(Op::StaleDirs).await;
        self.fake.stale_dirs().await
    }

    async fn remove_stale_dir(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate(Op::RemoveStaleDir).await;
        self.fake.remove_stale_dir(name).await
    }

    async fn create_volume(&self, spec: VolumeSpec) -> Result<VolumeInfo, ComputeError> {
        self.gate(Op::CreateVolume).await;
        self.fake.create_volume(spec).await
    }

    async fn volume(&self, name: &VolumeName) -> Result<Option<VolumeInfo>, ComputeError> {
        self.gate(Op::Volume).await;
        self.fake.volume(name).await
    }

    async fn list_volumes(&self) -> Result<Vec<VolumeInfo>, ComputeError> {
        self.gate(Op::ListVolumes).await;
        self.fake.list_volumes().await
    }

    async fn remove_volume(&self, name: &VolumeName) -> Result<(), ComputeError> {
        self.gate(Op::RemoveVolume).await;
        self.fake.remove_volume(name).await
    }
}
