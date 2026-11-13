// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A sdk that answers the plan it is given and records what was applied.
struct FakeSdk {
    plan: Result<GrowPlan, ()>,
    fail_apply: bool,
    applied: std::sync::Mutex<Vec<&'static str>>,
}

impl FakeSdk {
    fn new(plan: Result<GrowPlan, ()>, fail_apply: bool) -> Self {
        FakeSdk {
            plan,
            fail_apply,
            applied: std::sync::Mutex::new(Vec::new()),
        }
    }
    fn apply(&self, how: &'static str) -> Result<(), ComputeError> {
        self.applied.lock().unwrap().push(how);
        if self.fail_apply {
            Err(ComputeError::InvalidSpec {
                reason: "apply failed".into(),
            })
        } else {
            Ok(())
        }
    }
}

impl GrowSdk for FakeSdk {
    fn plan(&self) -> impl Future<Output = Result<GrowPlan, ComputeError>> + Send {
        std::future::ready(match &self.plan {
            Ok(GrowPlan::Refused(r)) => Ok(GrowPlan::Refused(r.clone())),
            Ok(GrowPlan::Live) => Ok(GrowPlan::Live),
            Ok(GrowPlan::Deferred) => Ok(GrowPlan::Deferred),
            Err(()) => Err(ComputeError::InvalidSpec {
                reason: "plan failed".into(),
            }),
        })
    }
    fn apply_live(&self) -> impl Future<Output = Result<(), ComputeError>> + Send {
        std::future::ready(self.apply("live"))
    }
    fn apply_next_start(&self) -> impl Future<Output = Result<(), ComputeError>> + Send {
        std::future::ready(self.apply("next start"))
    }
}

#[tokio::test]
async fn a_live_plan_applies_now_and_a_deferred_one_at_the_next_start() {
    let live = FakeSdk::new(Ok(GrowPlan::Live), false);
    assert_eq!(grow(&live).await.unwrap(), RootDiskGrowth::Live);
    assert_eq!(*live.applied.lock().unwrap(), ["live"]);
    let deferred = FakeSdk::new(Ok(GrowPlan::Deferred), false);
    assert_eq!(grow(&deferred).await.unwrap(), RootDiskGrowth::NextStart);
    assert_eq!(*deferred.applied.lock().unwrap(), ["next start"]);
}

#[tokio::test]
async fn a_refused_plan_applies_nothing_and_a_failed_step_is_the_error() {
    let refused = FakeSdk::new(Ok(GrowPlan::Refused("not above".into())), false);
    let err = grow(&refused).await.unwrap_err();
    assert!(matches!(err, ComputeError::InvalidSpec { ref reason } if reason == "not above"));
    assert!(refused.applied.lock().unwrap().is_empty());
    let no_plan = FakeSdk::new(Err(()), false);
    assert!(grow(&no_plan).await.is_err());
    assert!(no_plan.applied.lock().unwrap().is_empty());
    for plan in [GrowPlan::Live, GrowPlan::Deferred] {
        assert!(grow(&FakeSdk::new(Ok(plan), true)).await.is_err());
    }
}

/// A sandboxes folder with two sandboxes, `mine` (its `runtime/` folder made) and `other`
/// (with a root disk).
struct Home {
    _dir: tempfile::TempDir,
    sandboxes: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let sandboxes = dir.path().join("sandboxes");
        fs::create_dir_all(sandboxes.join("mine").join("runtime")).unwrap();
        fs::create_dir_all(sandboxes.join("other")).unwrap();
        fs::write(sandboxes.join("mine").join("upper.ext4"), b"mine").unwrap();
        fs::write(sandboxes.join("other").join("upper.ext4"), b"other").unwrap();
        Home {
            _dir: dir,
            sandboxes,
        }
    }

    fn mine(&self) -> PathBuf {
        self.sandboxes.join("mine")
    }

    /// Writes `mine`'s journal as msb does, with `layers`.
    fn journal(&self, layers: &[&Path]) {
        let layers: Vec<_> = layers
            .iter()
            .enumerate()
            .map(|(i, path)| {
                serde_json::json!({
                    "layer_id": format!("layer-{i:032x}"),
                    "path": path,
                    "format": if i == 0 { "raw" } else { "qcow2" },
                    "integrity_root": null,
                })
            })
            .collect();
        let journal = serde_json::json!({
            "schema": "microsandbox.runtime-root-disk/1",
            "volume_id": "vol-00000000000000000000000000000000",
            "device_id": "vdb",
            "layout": "managed-upper",
            "published_generation": 0,
            "layers": layers,
        });
        self.write_journal(&serde_json::to_vec(&journal).unwrap());
    }

    fn write_journal(&self, bytes: &[u8]) {
        fs::write(self.mine().join(JOURNAL), bytes).unwrap();
    }
}

/// A sparse file of `length` bytes with `data` bytes written at its start.
fn sparse(path: &Path, length: u64, data: usize) {
    use std::io::Write as _;
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&vec![7_u8; data]).unwrap();
    file.set_len(length).unwrap();
}

#[test]
fn a_raw_root_disk_is_as_full_as_its_image_is_allocated() {
    let home = Home::new();
    let upper = home.mine().join("upper.ext4");
    sparse(&upper, 64 * MIB, 3 * 1024 * 1024);
    let got = usage(&home.mine()).unwrap();
    assert_eq!(got.size, DiskSize::mib(64));
    assert!(got.used >= DiskSize::mib(3), "{got:?}");
    // NTFS allocates a file extended with `set_len` in full unless it is marked sparse, so
    // only file systems that make holes can show the file under its length.
    #[cfg(unix)]
    assert!(got.used < DiskSize::mib(64), "{got:?}");
}

#[test]
fn a_qcow2_head_gives_the_virtual_size_and_every_layer_counts() {
    let home = Home::new();
    let base = home.mine().join("runtime").join("base.raw");
    sparse(&base, 8 * MIB, 2 * 1024 * 1024);
    let head = home.mine().join("head.qcow2");
    let mut header = vec![0_u8; 32];
    header[..4].copy_from_slice(&QCOW2_MAGIC);
    header[24..32].copy_from_slice(&(16 * MIB).to_be_bytes());
    fs::write(&head, &header).unwrap();
    home.journal(&[&base, &head]);
    let got = usage(&home.mine()).unwrap();
    assert_eq!(got.size, DiskSize::mib(16));
    assert!(got.used >= DiskSize::mib(2), "{got:?}");
}

#[test]
fn no_disk_gives_no_figure() {
    let home = Home::new();
    fs::remove_file(home.mine().join("upper.ext4")).unwrap();
    assert_eq!(usage(&home.mine()), None);
}

fn plan(
    conflict: Option<&str>,
    disposition: Option<microsandbox::ModificationDisposition>,
    field: &str,
) -> microsandbox::sandbox::SandboxModificationPlan {
    use microsandbox::{
        ChangeKind, ConfigPlannedChange, ModificationConflict, ModificationPolicy, PlannedChange,
    };
    microsandbox::sandbox::SandboxModificationPlan {
        sandbox: "a".into(),
        status: "running".into(),
        applied: false,
        policy: ModificationPolicy::NoRestart,
        changes: disposition
            .map(|disposition| {
                PlannedChange::Config(ConfigPlannedChange {
                    field: field.into(),
                    change: ChangeKind::Updated,
                    before: None,
                    after: None,
                    disposition,
                    reason: None,
                })
            })
            .into_iter()
            .collect(),
        conflicts: conflict
            .map(|message| ModificationConflict {
                field: "root_disk_size".into(),
                message: message.into(),
            })
            .into_iter()
            .collect(),
        warnings: vec![],
        resize_status: vec![],
    }
}

#[test]
fn a_live_plan_defers_where_live_grow_does_not_work() {
    assert_eq!(on_platform(GrowPlan::Live, true), GrowPlan::Live);
    assert_eq!(on_platform(GrowPlan::Live, false), GrowPlan::Deferred);
    assert_eq!(on_platform(GrowPlan::Deferred, false), GrowPlan::Deferred);
    assert_eq!(
        on_platform(GrowPlan::Refused("x".into()), false),
        GrowPlan::Refused("x".into())
    );
    assert_eq!(LIVE_GROW_WORKS, !cfg!(windows));
}

#[test]
fn a_plan_reads_as_live_deferred_or_refused() {
    use microsandbox::ModificationDisposition as D;
    assert_eq!(
        read_plan(&plan(None, Some(D::Live), "root_disk_size")),
        GrowPlan::Live
    );
    assert_eq!(
        read_plan(&plan(None, Some(D::NextStart), "root_disk_size")),
        GrowPlan::Deferred
    );
    assert_eq!(
        read_plan(&plan(None, Some(D::RequiresRestart), "root_disk_size")),
        GrowPlan::Deferred
    );
    // A live change of some other field is not the disk.
    assert_eq!(
        read_plan(&plan(None, Some(D::Live), "cpus")),
        GrowPlan::Deferred
    );
    assert_eq!(read_plan(&plan(None, None, "")), GrowPlan::Deferred);
    assert_eq!(
        read_plan(&plan(Some("too small"), Some(D::Live), "root_disk_size")),
        GrowPlan::Refused("too small".into())
    );
    // What the runtime acts on: live only where live grow works.
    let live = plan(None, Some(D::Live), "root_disk_size");
    assert_eq!(
        grow_plan(&live),
        on_platform(GrowPlan::Live, LIVE_GROW_WORKS)
    );
}
