// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reconcile against a runtime whose calls stop answering (tier U, paused clock).
#![expect(
    clippy::unwrap_used,
    reason = "test helpers: a failed step fails the test"
)]

use std::collections::BTreeSet;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_compute::fake::{ExecContext, FakeRuntime, FakeSandbox};
use puddle_compute::{
    Capabilities, ComputeError, DiskSize, ExecOutput, ExecRequest, ImageConfig, RootDiskGrowth,
    Runtime, Sandbox, SandboxInfo, SandboxSpec, VolumeInfo, VolumeSpec,
};
use puddle_lifecycle::{Inventory, ShutdownConfig, reconcile};
use puddle_types::{
    CpuCount, GuestEnv, ImageRef, MemoryMib, SandboxName, VolumeName, WorkspaceId, WorkspaceStatus,
};

fn name(s: &str) -> SandboxName {
    SandboxName::new(s).unwrap()
}

fn vol(s: &str) -> VolumeName {
    VolumeName::new(s).unwrap()
}

fn spec(n: &str) -> SandboxSpec {
    SandboxSpec::new(name(n), ImageRef::new(FakeRuntime::DEBIAN).unwrap())
}

const LIMIT: Duration = Duration::from_secs(5);

fn config() -> ShutdownConfig {
    ShutdownConfig {
        trim_timeout: Duration::from_secs(1),
        stop_timeout: LIMIT,
    }
}

/// The fake, except that the calls named in `stalled` never answer (a hung runtime). A stalled
/// call that names a target hangs only for that target.
#[derive(Clone)]
struct Stalling {
    inner: FakeRuntime,
    stalled: Arc<Mutex<BTreeSet<(&'static str, String)>>>,
}

impl Stalling {
    fn new(inner: &FakeRuntime) -> Self {
        Self {
            inner: inner.clone(),
            stalled: Arc::default(),
        }
    }

    fn stall(&self, op: &'static str, target: &str) {
        self.stalled.lock().unwrap().insert((op, target.to_owned()));
    }

    async fn gate(&self, op: &'static str, target: &str) {
        let hangs = self
            .stalled
            .lock()
            .unwrap()
            .contains(&(op, target.to_owned()));
        if hangs {
            std::future::pending::<()>().await;
        }
    }
}

impl Runtime for Stalling {
    type Sandbox = FakeSandbox;

    fn probe(&self) -> impl Future<Output = Result<Capabilities, ComputeError>> + Send {
        self.inner.probe()
    }

    fn pull_image(
        &self,
        image: &ImageRef,
    ) -> impl Future<Output = Result<ImageConfig, ComputeError>> + Send {
        self.inner.pull_image(image)
    }

    fn create(
        &self,
        spec: SandboxSpec,
    ) -> impl Future<Output = Result<FakeSandbox, ComputeError>> + Send {
        self.inner.create(spec)
    }

    fn start(
        &self,
        name: &SandboxName,
    ) -> impl Future<Output = Result<FakeSandbox, ComputeError>> + Send {
        self.inner.start(name)
    }

    async fn get(&self, name: &SandboxName) -> Result<FakeSandbox, ComputeError> {
        self.gate("get", name.as_str()).await;
        self.inner.get(name).await
    }

    fn set_memory(
        &self,
        name: &SandboxName,
        memory: MemoryMib,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        self.inner.set_memory(name, memory)
    }

    fn set_cpus(
        &self,
        name: &SandboxName,
        cpus: CpuCount,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        self.inner.set_cpus(name, cpus)
    }

    fn set_env(
        &self,
        name: &SandboxName,
        env: &GuestEnv,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        self.inner.set_env(name, env)
    }

    fn grow_root_disk(
        &self,
        name: &SandboxName,
        size: DiskSize,
    ) -> impl Future<Output = Result<RootDiskGrowth, ComputeError>> + Send {
        self.inner.grow_root_disk(name, size)
    }

    async fn list(&self) -> Result<Vec<SandboxInfo>, ComputeError> {
        self.gate("list", "").await;
        self.inner.list().await
    }

    async fn remove(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate("remove", name.as_str()).await;
        self.inner.remove(name).await
    }

    async fn kill(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate("kill", name.as_str()).await;
        self.inner.kill(name).await
    }

    async fn stale_dirs(&self) -> Result<Vec<String>, ComputeError> {
        self.gate("stale_dirs", "").await;
        self.inner.stale_dirs().await
    }

    async fn remove_stale_dir(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.gate("remove_stale_dir", name.as_str()).await;
        self.inner.remove_stale_dir(name).await
    }

    fn create_volume(
        &self,
        spec: VolumeSpec,
    ) -> impl Future<Output = Result<VolumeInfo, ComputeError>> + Send {
        self.inner.create_volume(spec)
    }

    fn volume(
        &self,
        name: &VolumeName,
    ) -> impl Future<Output = Result<Option<VolumeInfo>, ComputeError>> + Send {
        self.inner.volume(name)
    }

    async fn list_volumes(&self) -> Result<Vec<VolumeInfo>, ComputeError> {
        self.gate("list_volumes", "").await;
        self.inner.list_volumes().await
    }

    async fn remove_volume(&self, name: &VolumeName) -> Result<(), ComputeError> {
        self.gate("remove_volume", name.as_str()).await;
        self.inner.remove_volume(name).await
    }
}

fn fstrim_ok(rt: &FakeRuntime) {
    rt.on_exec(|_: &mut ExecContext<'_>, req: &ExecRequest| {
        (req.program == "fstrim" || req.args.iter().any(|a| a.contains("fstrim")))
            .then(|| ExecOutput::new(0, "", ""))
    });
}

fn failed(report: &puddle_lifecycle::ReconcileReport) -> Vec<(&str, &str)> {
    report
        .failures
        .iter()
        .map(|f| (f.item.as_str(), f.action))
        .collect()
}

fn says_no_answer(report: &puddle_lifecycle::ReconcileReport) {
    assert!(
        report
            .failures
            .first()
            .is_some_and(|f| f.error.contains("no answer within 5s")),
        "{:?}",
        report.failures
    );
}

#[tokio::test(start_paused = true)]
async fn a_list_that_never_answers_ends_reconcile_with_an_error_after_the_limit() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    rt.stall("list", "");
    let start = tokio::time::Instant::now();
    let err = reconcile(&rt, &Inventory::default(), &config())
        .await
        .unwrap_err();
    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(
        err,
        ComputeError::Runtime {
            op: "list",
            message: "no answer within 5s".into()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn a_get_that_never_answers_fails_that_orphan_and_the_rest_goes_on() {
    let fake = FakeRuntime::new();
    fstrim_ok(&fake);
    let rt = Stalling::new(&fake);
    let _hung = fake.create(spec("hung")).await.unwrap();
    let _fine = fake.create(spec("fine")).await.unwrap();
    rt.stall("get", "hung");
    fake.add_stale_dir("failed-create");

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &Inventory::default(), &config())
        .await
        .unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("hung", "stop")]);
    says_no_answer(&report);
    assert_eq!(report.stopped, [name("fine")]);
    assert_eq!(report.removed, [name("fine")]);
    assert_eq!(report.stale_dirs_removed, [name("failed-create")]);
}

#[tokio::test(start_paused = true)]
async fn a_kill_that_never_answers_fails_that_orphan() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    let _stuck = fake.create(spec("stuck")).await.unwrap();
    assert!(fake.set_status(&name("stuck"), WorkspaceStatus::Paused));
    rt.stall("kill", "stuck");

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &Inventory::default(), &config())
        .await
        .unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("stuck", "kill")]);
    says_no_answer(&report);
    assert!(report.stopped.is_empty() && report.removed.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_remove_that_never_answers_fails_that_record_and_the_rest_goes_on() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    for n in ["a-stale", "b-stale"] {
        fake.create(spec(n)).await.unwrap().stop().await.unwrap();
    }
    rt.stall("remove", "a-stale");

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &Inventory::default(), &config())
        .await
        .unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("a-stale", "remove")]);
    says_no_answer(&report);
    assert_eq!(report.removed, [name("b-stale")]);
}

#[tokio::test(start_paused = true)]
async fn a_stale_dir_listing_that_never_answers_keeps_the_stops_and_still_checks_volumes() {
    let fake = FakeRuntime::new();
    fstrim_ok(&fake);
    let rt = Stalling::new(&fake);
    let _left = fake.create(spec("left")).await.unwrap();
    fake.create_volume(VolumeSpec {
        name: vol("ws-half"),
        size: DiskSize::mib(8),
    })
    .await
    .unwrap();
    rt.stall("stale_dirs", "");
    let inventory = Inventory {
        interrupted: BTreeSet::from([WorkspaceId::new("half").unwrap()]),
        ..Inventory::default()
    };

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &inventory, &config()).await.unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("stale directories", "list stale dirs")]);
    says_no_answer(&report);
    assert_eq!(report.stopped, [name("left")]);
    assert_eq!(report.volumes_removed, [vol("ws-half")]);
}

#[tokio::test(start_paused = true)]
async fn a_volume_listing_that_never_answers_is_a_failure_not_a_missing_volume() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    rt.stall("list_volumes", "");
    let inventory = Inventory {
        workspaces: BTreeSet::from([WorkspaceId::new("known").unwrap()]),
        ..Inventory::default()
    };

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &inventory, &config()).await.unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("volumes", "list volumes")]);
    says_no_answer(&report);
    assert_eq!(report.missing_volumes, []);
}

#[tokio::test(start_paused = true)]
async fn a_stale_dir_remove_that_never_answers_fails_that_dir_and_the_rest_goes_on() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    fake.add_stale_dir("a-dir");
    fake.add_stale_dir("b-dir");
    rt.stall("remove_stale_dir", "a-dir");

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &Inventory::default(), &config())
        .await
        .unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("a-dir", "remove stale dir")]);
    says_no_answer(&report);
    assert_eq!(report.stale_dirs_removed, [name("b-dir")]);
}

#[tokio::test(start_paused = true)]
async fn a_volume_remove_that_never_answers_fails_that_volume_and_the_rest_goes_on() {
    let fake = FakeRuntime::new();
    let rt = Stalling::new(&fake);
    for v in ["ws-a", "ws-b"] {
        fake.create_volume(VolumeSpec {
            name: vol(v),
            size: DiskSize::mib(8),
        })
        .await
        .unwrap();
    }
    rt.stall("remove_volume", "ws-a");
    let inventory = Inventory {
        interrupted: ["a", "b"]
            .map(|w| WorkspaceId::new(w).unwrap())
            .into_iter()
            .collect(),
        ..Inventory::default()
    };

    let start = tokio::time::Instant::now();
    let report = reconcile(&rt, &inventory, &config()).await.unwrap();

    assert_eq!(start.elapsed(), LIMIT);
    assert_eq!(failed(&report), [("ws-a", "remove volume")]);
    says_no_answer(&report);
    assert_eq!(report.volumes_removed, [vol("ws-b")]);
}
