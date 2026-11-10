// SPDX-License-Identifier: GPL-3.0-or-later
//! How full a sandbox's root disk is, read from msb's root-disk journal.
//!
//! msb keeps `root-disk.json` in the sandbox's own folder, out of the guest's reach (the copy in
//! `runtime/` is only a guard the guest cannot use), so the files it names are the host's to read.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

use puddle_compute::{ComputeError, DiskSize, RootDiskGrowth, RootDiskUse};
use serde::Deserialize;

/// The journal's file name in the sandbox's folder (msb's name for it).
const JOURNAL: &str = "root-disk.json";

/// The part of msb's journal that names the disk files; its other fields are not needed here.
#[derive(Deserialize)]
struct Journal {
    /// The disk files, oldest first, the last one the writable head.
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    path: PathBuf,
}

/// How full the root disk of the sandbox whose folder is `sandbox_dir` is: its capacity and what the files that make up the disk take on the host.
/// `None` when there is no disk to look at (not made yet, a layout this does not know, or a
/// sandbox of the older layout, whose journal msb has not yet moved out of `runtime/`).
pub(crate) fn usage(sandbox_dir: &Path) -> Option<RootDiskUse> {
    let files = disk_files(sandbox_dir)?;
    let size = capacity(files.last()?)?;
    let mut used: u64 = 0;
    for file in &files {
        used = used.saturating_add(puddle_fs::allocated_bytes(file).ok()?);
    }
    let size = to_mib(size);
    // A qcow2 head can take more than the base's length; the disk is then as full as it can be.
    let used = to_mib(used.div_ceil(MIB) * MIB).min(size);
    Some(RootDiskUse {
        size: DiskSize::mib(size),
        used: DiskSize::mib(used),
    })
}

/// What msb's plan for a root-disk size change says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GrowPlan {
    /// msb refuses the change; the text says why (a size that is not above the current one).
    Refused(String),
    /// The running guest gets the room now.
    Live,
    /// The room arrives at the next start (a stopped sandbox, or a runtime that cannot grow a
    /// running disk).
    Deferred,
}

/// Reads msb's dry-run `plan` of a root-disk size change, as this platform can carry it out.
pub(crate) fn grow_plan(plan: &microsandbox::sandbox::SandboxModificationPlan) -> GrowPlan {
    on_platform(read_plan(plan), LIVE_GROW_WORKS)
}

/// What msb's dry-run `plan` says, whatever the platform.
fn read_plan(plan: &microsandbox::sandbox::SandboxModificationPlan) -> GrowPlan {
    use microsandbox::{ModificationDisposition, PlannedChange};
    if let Some(conflict) = plan.conflicts.first() {
        return GrowPlan::Refused(conflict.message.clone());
    }
    let live = plan.changes.iter().any(|change| {
        matches!(
            change,
            PlannedChange::Config(c)
                if c.field == "root_disk_size" && c.disposition == ModificationDisposition::Live
        )
    });
    if live {
        GrowPlan::Live
    } else {
        GrowPlan::Deferred
    }
}

/// Whether the runtime's hypervisor can take a live grow. msb plans one on Windows too, but the
/// pinned libkrun fork keeps the raw disk handle at its old length there, so the guest's online
/// ext4 expansion fails with an I/O error and leaves the block device grown and the filesystem
/// not (seen in vm-windows CI). Live grow on Windows waits for the fork fix
/// (`upstream-issues/libkrun-windows-live-grow/`); until then the disk grows at a restart.
pub(crate) const LIVE_GROW_WORKS: bool = !cfg!(windows);

/// `plan` as this platform can carry it out: a live plan becomes a deferred one where
/// `live_works` is false, so the host restarts the workspace instead of growing it under the guest.
pub(crate) fn on_platform(plan: GrowPlan, live_works: bool) -> GrowPlan {
    match plan {
        GrowPlan::Live if !live_works => GrowPlan::Deferred,
        other => other,
    }
}

/// The three msb calls of a root-disk grow, so the choice between them is tested without a real
/// msb database.
pub(crate) trait GrowSdk {
    /// msb's dry run of the change.
    fn plan(&self) -> impl Future<Output = Result<GrowPlan, ComputeError>> + Send;
    /// The change on a running guest.
    fn apply_live(&self) -> impl Future<Output = Result<(), ComputeError>> + Send;
    /// The change at the next start.
    fn apply_next_start(&self) -> impl Future<Output = Result<(), ComputeError>> + Send;
}

/// Grows through `sdk`: a refused plan is an invalid spec, a live plan applies now, any other at
/// the next start.
pub(crate) async fn grow(sdk: &impl GrowSdk) -> Result<RootDiskGrowth, ComputeError> {
    match sdk.plan().await? {
        GrowPlan::Refused(reason) => Err(ComputeError::InvalidSpec { reason }),
        GrowPlan::Live => sdk.apply_live().await.map(|()| RootDiskGrowth::Live),
        GrowPlan::Deferred => sdk
            .apply_next_start()
            .await
            .map(|()| RootDiskGrowth::NextStart),
    }
}

const MIB: u64 = 1024 * 1024;

/// qcow2 files start with this magic, then the version and the backing-file fields; the virtual
/// size is the big-endian `u64` at byte 24.
const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";

/// What the disk file `head` can hold: a qcow2 file's virtual size, else (a raw disk) its length.
fn capacity(head: &Path) -> Option<u64> {
    use std::io::Read as _;
    let mut file = fs::File::open(head).ok()?;
    let length = file.metadata().ok()?.len();
    let mut header = [0_u8; 32];
    if file.read_exact(&mut header).is_ok() && header[..4] == QCOW2_MAGIC {
        let size: [u8; 8] = header[24..32].try_into().ok()?;
        return Some(u64::from_be_bytes(size));
    }
    Some(length)
}

fn to_mib(bytes: u64) -> u32 {
    u32::try_from(bytes / MIB).unwrap_or(u32::MAX)
}

/// The files of the root disk, oldest first: the journal's layers when msb keeps one, else the
/// plain `upper.ext4` of a managed root disk.
fn disk_files(sandbox_dir: &Path) -> Option<Vec<PathBuf>> {
    let journal = sandbox_dir.join(JOURNAL);
    if journal.exists() {
        let bytes = fs::read(&journal).ok()?;
        let parsed: Journal = serde_json::from_slice(&bytes).ok()?;
        let files: Vec<PathBuf> = parsed.layers.into_iter().map(|l| l.path).collect();
        return (!files.is_empty()).then_some(files);
    }
    let upper = sandbox_dir.join("upper.ext4");
    upper.is_file().then(|| vec![upper])
}

#[cfg(test)]
mod tests {
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
            ChangeKind, ConfigPlannedChange, ModificationConflict, ModificationPolicy,
            PlannedChange,
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
}
