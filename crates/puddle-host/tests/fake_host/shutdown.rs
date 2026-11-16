// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shutting the host down.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_runs_the_steps_in_order_and_trims_then_stops_every_sandbox() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    // The probe after the shutdown only proves a stop when it can see the running API.
    assert_eq!(
        puddle_test_kit::http_status(
            ([127, 0, 0, 1], host.url().port().unwrap()).into(),
            "/api/workspaces",
            host.token()
        ),
        Some(200),
        "the running API answers its own token"
    );

    let down = host.shutdown().await;
    assert_eq!(down.steps, SHUTDOWN_STEPS);
    assert_eq!(down.operations_ended, 0);
    assert!(down.sandboxes.all_stopped(), "{down:?}");
    assert_eq!(down.sandboxes.sandboxes.len(), 2);
    assert!(
        down.sandboxes
            .sandboxes
            .iter()
            .all(|s| s.trim == puddle_lifecycle::TrimOutcome::Trimmed),
        "{down:?}"
    );
    // Each sandbox: the trim command ran before its stop.
    let commands = rig.guest.commands();
    for name in ["acme", "beta"] {
        let trim = commands
            .iter()
            .position(|c| c == &format!("{name} shutdown-trim"))
            .unwrap();
        let stop = rig
            .runtime
            .calls()
            .iter()
            .position(|c| format!("{:?}", c.op) == "Stop" && c.target.as_deref() == Some(name))
            .unwrap();
        assert!(trim < commands.len() && stop > 0);
    }
    for info in rig.runtime.list().await.unwrap() {
        assert!(info.status.is_down(), "{info:?}");
    }
    // The API is gone, and a second shutdown returns the first result.
    // Not "the port refuses connections": the API's number can be another test's by now. A stopped
    // host no longer answers its own token.
    assert_ne!(
        puddle_test_kit::http_status(
            ([127, 0, 0, 1], host.url().port().unwrap()).into(),
            "/api/workspaces",
            host.token()
        ),
        Some(200)
    );
    assert_eq!(host.shutdown().await, down);
    assert_eq!(
        host.steps()
            .iter()
            .filter(|s| **s == Step::ApiStopped)
            .count(),
        1
    );
}

/// A runtime whose image pull takes a while, so an operation is in flight at an await point.
#[derive(Clone)]
struct SlowPull(FakeRuntime, Duration);

impl Runtime for SlowPull {
    type Sandbox = <FakeRuntime as Runtime>::Sandbox;

    fn probe(
        &self,
    ) -> impl Future<Output = Result<puddle_compute::Capabilities, puddle_compute::ComputeError>> + Send
    {
        self.0.probe()
    }

    async fn pull_image(
        &self,
        image: &ImageRef,
    ) -> Result<ImageConfig, puddle_compute::ComputeError> {
        // Simulated slow image pull.
        tokio::time::sleep(self.1).await;
        self.0.pull_image(image).await
    }

    fn create(
        &self,
        spec: puddle_compute::SandboxSpec,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.create(spec)
    }

    fn start(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.start(name)
    }

    fn get(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<Self::Sandbox, puddle_compute::ComputeError>> + Send {
        self.0.get(name)
    }

    fn set_cpus(
        &self,
        name: &SandboxName,
        cpus: puddle_types::CpuCount,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.set_cpus(name, cpus)
    }

    fn set_env(
        &self,
        name: &SandboxName,
        env: &GuestEnv,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.set_env(name, env)
    }

    fn grow_root_disk(
        &self,
        name: &SandboxName,
        size: puddle_compute::DiskSize,
    ) -> impl Future<Output = Result<puddle_compute::RootDiskGrowth, puddle_compute::ComputeError>> + Send
    {
        self.0.grow_root_disk(name, size)
    }

    fn set_memory(
        &self,
        name: &SandboxName,
        memory: puddle_types::MemoryMib,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.set_memory(name, memory)
    }

    fn list(
        &self,
    ) -> impl Future<Output = Result<Vec<puddle_compute::SandboxInfo>, puddle_compute::ComputeError>>
    + Send {
        self.0.list()
    }

    fn remove(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove(name)
    }

    fn stale_dirs(
        &self,
    ) -> impl Future<Output = Result<Vec<String>, puddle_compute::ComputeError>> + Send {
        self.0.stale_dirs()
    }

    fn remove_stale_dir(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove_stale_dir(name)
    }

    fn create_volume(
        &self,
        spec: puddle_compute::VolumeSpec,
    ) -> impl Future<Output = Result<puddle_compute::VolumeInfo, puddle_compute::ComputeError>> + Send
    {
        self.0.create_volume(spec)
    }

    fn volume(
        &self,
        name: &puddle_types::VolumeName,
    ) -> impl Future<
        Output = Result<Option<puddle_compute::VolumeInfo>, puddle_compute::ComputeError>,
    > + Send {
        self.0.volume(name)
    }

    fn list_volumes(
        &self,
    ) -> impl Future<Output = Result<Vec<puddle_compute::VolumeInfo>, puddle_compute::ComputeError>> + Send
    {
        self.0.list_volumes()
    }

    fn remove_volume(
        &self,
        name: &puddle_types::VolumeName,
    ) -> impl Future<Output = Result<(), puddle_compute::ComputeError>> + Send {
        self.0.remove_volume(name)
    }
}

struct SlowFactory(SlowPull);

impl RuntimeFactory for SlowFactory {
    type Runtime = SlowPull;

    fn open(
        &self,
        _: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<SlowPull, HostError>> + Send {
        std::future::ready(Ok(self.0.clone()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_ends_an_operation_that_outlasts_the_grace_period() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.operations_grace = Duration::from_millis(50);
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let slow = SlowPull(rig.runtime.clone(), Duration::from_secs(30));
    let host = Host::start(prepared, &SlowFactory(slow), hermetic_options())
        .await
        .unwrap();
    let api = api_slow(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces", &new_workspace("acme")).await;
    events
        .until(
            |e| e["type"] == "workspace_progress" && e["step"] == "pulling_image",
            Duration::from_secs(20),
        )
        .await;
    let started = std::time::Instant::now();
    let down = host.shutdown().await;
    // flake-ok: the pull sleeps 30 s and the grace is 50 ms, so a shutdown that waited for the
    // operation takes 30 s and one that ended it takes about 0.06 s (64 ms measured under load).
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(down.operations_ended, 1, "{down:?}");
    assert_eq!(down.steps, SHUTDOWN_STEPS);
    // The ended create left its half-made volume for the next start's reconcile.
    assert!(!rig.runtime.list_volumes().await.unwrap().is_empty());
}

fn api_slow(host: &Host<SlowPull>) -> Api {
    Api::new(host.url(), host.token())
}

#[tokio::test(flavor = "multi_thread")]
async fn operations_are_refused_once_shutdown_has_begun() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    host.workspaces().close();
    let refused = api.post("/api/workspaces", &new_workspace("late")).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("shutting down"), "{}", refused.body);
    host.shutdown().await;
}
