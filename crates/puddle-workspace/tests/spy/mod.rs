// SPDX-License-Identifier: AGPL-3.0-or-later
//! A runtime that wraps the fake and shows a test two things the fake cannot: what the caller's
//! state is while a call runs, and a call that never answers (a request that is dropped).
//!
//! The fake answers every call at once, so a future built on it cannot be dropped half way.
//! Here a hook sees each runtime call before the fake does and says whether it goes through.

use std::future::Future;
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

    fn probe(&self) -> impl Future<Output = Result<Capabilities, ComputeError>> + Send {
        async move {
            self.gate(Op::Probe).await;
            self.fake.probe().await
        }
    }

    fn pull_image(
        &self,
        image: &ImageRef,
    ) -> impl Future<Output = Result<ImageConfig, ComputeError>> + Send {
        async move {
            self.gate(Op::PullImage).await;
            self.fake.pull_image(image).await
        }
    }

    fn create(
        &self,
        spec: SandboxSpec,
    ) -> impl Future<Output = Result<Self::Sandbox, ComputeError>> + Send {
        async move {
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
    }

    fn start(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, ComputeError>> + Send {
        async move {
            self.gate(Op::Start).await;
            self.fake.start(name).await
        }
    }

    fn get(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, ComputeError>> + Send {
        async move {
            self.gate(Op::Get).await;
            self.fake.get(name).await
        }
    }

    fn set_memory(
        &self,
        name: &SandboxName,
        memory: MemoryMib,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::SetMemory).await;
            self.fake.set_memory(name, memory).await
        }
    }

    fn set_cpus(
        &self,
        name: &SandboxName,
        cpus: CpuCount,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::SetCpus).await;
            self.fake.set_cpus(name, cpus).await
        }
    }

    fn set_env(
        &self,
        name: &SandboxName,
        env: &GuestEnv,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::SetEnv).await;
            self.fake.set_env(name, env).await
        }
    }

    fn grow_root_disk(
        &self,
        name: &SandboxName,
        size: DiskSize,
    ) -> impl Future<Output = Result<RootDiskGrowth, ComputeError>> + Send {
        async move {
            self.gate(Op::GrowRootDisk).await;
            self.fake.grow_root_disk(name, size).await
        }
    }

    fn list(&self) -> impl Future<Output = Result<Vec<SandboxInfo>, ComputeError>> + Send {
        async move {
            self.gate(Op::List).await;
            self.fake.list().await
        }
    }

    fn remove(&self, name: &SandboxName) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::Remove).await;
            self.fake.remove(name).await
        }
    }

    fn kill(&self, name: &SandboxName) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::Kill).await;
            self.fake.kill(name).await
        }
    }

    fn stale_dirs(&self) -> impl Future<Output = Result<Vec<String>, ComputeError>> + Send {
        async move {
            self.gate(Op::StaleDirs).await;
            self.fake.stale_dirs().await
        }
    }

    fn remove_stale_dir(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::RemoveStaleDir).await;
            self.fake.remove_stale_dir(name).await
        }
    }

    fn create_volume(
        &self,
        spec: VolumeSpec,
    ) -> impl Future<Output = Result<VolumeInfo, ComputeError>> + Send {
        async move {
            self.gate(Op::CreateVolume).await;
            self.fake.create_volume(spec).await
        }
    }

    fn volume(
        &self,
        name: &VolumeName,
    ) -> impl Future<Output = Result<Option<VolumeInfo>, ComputeError>> + Send {
        async move {
            self.gate(Op::Volume).await;
            self.fake.volume(name).await
        }
    }

    fn list_volumes(&self) -> impl Future<Output = Result<Vec<VolumeInfo>, ComputeError>> + Send {
        async move {
            self.gate(Op::ListVolumes).await;
            self.fake.list_volumes().await
        }
    }

    fn remove_volume(
        &self,
        name: &VolumeName,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        async move {
            self.gate(Op::RemoveVolume).await;
            self.fake.remove_volume(name).await
        }
    }
}
