// SPDX-License-Identifier: GPL-3.0-or-later
//! Starting a workspace from checkouts on this computer, against the fake runtime: the import
//! runs in the workspace's maintenance sandbox with no route, the volume and each checkout's
//! `.git` read-only, clones each one with every git call guarded, and leaves nothing behind.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use puddle_compute::fake::{ExecContext, FakeRuntime, Fault, Op};
use puddle_compute::{ComputeError, DiskSize, ExecOutput, ExecRequest, Runtime};
use puddle_types::{CheckoutDir, GuestPath, ImageRef, WorkspaceId};
use puddle_workspace::{FINISH_IMPORT_SH, Import, WorkspaceError, Workspaces};

/// A git directory by structure at `<base>/<name>/.git`, its HEAD naming `branch`.
fn checkout(base: &Path, name: &str, branch: &str) -> PathBuf {
    let git = base.join(name).join(".git");
    std::fs::create_dir_all(git.join("objects")).unwrap();
    std::fs::create_dir_all(git.join("refs")).unwrap();
    std::fs::write(git.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
    git
}

fn import(git_dir: PathBuf, dir: &str) -> Import {
    Import {
        git_dir,
        directory: CheckoutDir::new(dir).unwrap(),
        url: format!("https://github.com/acme/{dir}.git"),
    }
}

/// What the guest was asked, one line each: the sandbox, the user, the program and its
/// arguments; and what each clone read of its mounted source's HEAD.
#[derive(Clone, Default)]
struct Guest {
    log: Arc<Mutex<Vec<String>>>,
    heads: Arc<Mutex<Vec<String>>>,
    fail_clone_of: Arc<Mutex<Option<String>>>,
    /// The finishing step for this source fails, and the `rm` after it answers this code.
    fail_finish_of: Arc<Mutex<Option<(String, i32)>>>,
}

impl Guest {
    fn install(&self, rt: &FakeRuntime) {
        let g = self.clone();
        rt.on_exec(move |ctx: &mut ExecContext<'_>, r: &ExecRequest| {
            g.log.lock().unwrap().push(format!(
                "{} {} {} {}",
                ctx.sandbox(),
                r.user.as_deref().unwrap_or("-"),
                r.program,
                r.args.join(" ")
            ));
            if r.program == "git" && r.args.contains(&"clone".to_owned()) {
                let src = r.args.iter().rev().nth(1).unwrap();
                let head = ctx
                    .read(&GuestPath::new(&format!("{src}/.git/HEAD")).unwrap())
                    .unwrap();
                g.heads
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(head).unwrap());
                if g.fail_clone_of.lock().unwrap().as_deref() == Some(src.as_str()) {
                    return Some(ExecOutput::new(128, "", "fatal: bad object HEAD\n"));
                }
            }
            if let Some((src, rm_code)) = g.fail_finish_of.lock().unwrap().clone() {
                if r.program == "sh" && r.args.get(4) == Some(&src) {
                    return Some(ExecOutput::new(1, "", "fatal: couldn't find remote ref\n"));
                }
                if r.program == "rm" {
                    return Some(ExecOutput::new(rm_code, "", "rm: cannot remove: busy\n"));
                }
            }
            Some(ExecOutput::new(0, "", ""))
        });
    }

    fn lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

struct Setup {
    rt: FakeRuntime,
    guest: Guest,
    workspaces: Workspaces,
    id: WorkspaceId,
    _tmp: tempfile::TempDir,
    base: PathBuf,
}

fn setup() -> Setup {
    let rt = FakeRuntime::new();
    let guest = Guest::default();
    guest.install(&rt);
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().to_path_buf();
    Setup {
        rt,
        guest,
        workspaces: Workspaces::default(),
        id: WorkspaceId::new("acme").unwrap(),
        _tmp: tmp,
        base,
    }
}

fn image() -> ImageRef {
    ImageRef::new(FakeRuntime::DEBIAN).unwrap()
}

async fn sandboxes(rt: &FakeRuntime) -> Vec<String> {
    rt.list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect()
}

#[tokio::test]
async fn each_checkout_is_cloned_in_a_sandbox_with_no_route_that_is_gone_afterwards() {
    let s = setup();
    let imports = [
        import(checkout(&s.base, "api", "main"), "api"),
        import(checkout(&s.base, "web", "feature"), "web"),
    ];
    let report = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap();
    assert!(report.created_volume);
    assert_eq!(report.failed, []);
    // The clones read the checkouts' own `.git` through the mounts, in order.
    assert_eq!(
        *s.guest.heads.lock().unwrap(),
        ["ref: refs/heads/main\n", "ref: refs/heads/feature\n"]
    );
    let lines = s.guest.lines();
    assert_eq!(
        lines,
        [
            "m--acme - git clone --quiet --no-local --no-checkout --origin puddle-checkout -- /puddle/import/0 /workspaces/acme/api".to_owned(),
            format!("m--acme - sh -c {FINISH_IMPORT_SH} puddle-import /workspaces/acme/api /puddle/import/0 https://github.com/acme/api.git"),
            "m--acme - git clone --quiet --no-local --no-checkout --origin puddle-checkout -- /puddle/import/1 /workspaces/acme/web".to_owned(),
            format!("m--acme - sh -c {FINISH_IMPORT_SH} puddle-import /workspaces/acme/web /puddle/import/1 https://github.com/acme/web.git"),
            "m--acme root sync ".to_owned(),
        ]
    );
    assert_eq!(sandboxes(&s.rt).await, Vec::<String>::new());
    let volumes: Vec<String> =
        s.rt.list_volumes()
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
    assert_eq!(volumes, ["ws-acme"]);
    // The workspace's own sandbox may take the volume now.
    let spec =
        puddle_compute::SandboxSpec::new(puddle_types::SandboxName::new("acme").unwrap(), image());
    s.workspaces
        .create(&s.rt, &s.id, spec, Some(DiskSize::gib(1)))
        .await
        .unwrap();
}

#[tokio::test]
async fn the_working_tree_is_never_what_is_mounted() {
    let s = setup();
    let git = checkout(&s.base, "api", "main");
    // A caller that passes the folder instead of its `.git` is refused before anything boots.
    let folder = git.parent().unwrap().to_path_buf();
    let err = s
        .workspaces
        .import(
            &s.rt,
            &s.id,
            &image(),
            DiskSize::gib(1),
            &[import(folder, "api")],
        )
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("is not a `.git` directory"),
        "{err}"
    );
    assert_eq!(s.guest.lines(), Vec::<String>::new());
    assert_eq!(sandboxes(&s.rt).await, Vec::<String>::new());
    // The volume it would have made is not left behind.
    assert_eq!(s.rt.list_volumes().await.unwrap(), []);
}

#[tokio::test]
async fn a_checkout_git_refuses_fails_alone_and_leaves_no_folder() {
    let s = setup();
    *s.guest.fail_clone_of.lock().unwrap() = Some("/puddle/import/0".into());
    let imports = [
        import(checkout(&s.base, "api", "main"), "api"),
        import(checkout(&s.base, "web", "main"), "web"),
    ];
    let report = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0.as_str(), "api");
    assert!(
        report.failed[0]
            .1
            .contains("git exited 128: fatal: bad object HEAD"),
        "{:?}",
        report.failed
    );
    // git removed its own refused clone: no `rm` of a path git owns.
    let lines = s.guest.lines();
    assert!(!lines.iter().any(|l| l.contains(" rm ")), "{lines:#?}");
    assert_eq!(lines.iter().filter(|l| l.contains(" sh -c ")).count(), 1);
    assert_eq!(lines.last().unwrap(), "m--acme root sync ");
}

#[tokio::test]
async fn a_clone_whose_finishing_step_fails_is_removed_and_a_failed_removal_is_said() {
    for (rm_code, said) in [(0, false), (1, true)] {
        let s = setup();
        *s.guest.fail_finish_of.lock().unwrap() = Some(("/puddle/import/0".into(), rm_code));
        let report = s
            .workspaces
            .import(
                &s.rt,
                &s.id,
                &image(),
                DiskSize::gib(1),
                &[import(checkout(&s.base, "api", "main"), "api")],
            )
            .await
            .unwrap();
        let (dir, reason) = &report.failed[0];
        assert_eq!(dir.as_str(), "api");
        assert!(
            reason.starts_with("git exited 1: fatal: couldn't find remote ref"),
            "{reason}"
        );
        assert_eq!(
            reason.contains("half-made folder was not removed"),
            said,
            "{reason}"
        );
        assert!(
            s.guest
                .lines()
                .contains(&"m--acme root rm -rf -- /workspaces/acme/api".to_owned())
        );
    }
}

#[tokio::test]
async fn a_clone_that_runs_out_of_time_fails_alone_and_a_lost_guest_fails_the_import() {
    let s = setup();
    s.rt.inject(
        Op::Exec,
        Fault::once(ComputeError::ExecTimeout {
            sandbox: "m--acme".into(),
            program: "git".into(),
            timeout: std::time::Duration::from_secs(600),
        }),
    );
    let imports = [
        import(checkout(&s.base, "api", "main"), "api"),
        import(checkout(&s.base, "web", "main"), "web"),
    ];
    let report = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].1, "git did not finish within 600 s");
    assert!(report.created_volume);

    // The guest gone mid-import: the create fails, and the volume it made goes with it.
    let s = setup();
    s.rt.inject(
        Op::Exec,
        Fault::once(ComputeError::Runtime {
            op: "exec",
            message: "the guest is gone".into(),
        }),
    );
    let err = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("the guest is gone"), "{err}");
    assert_eq!(s.rt.list_volumes().await.unwrap(), []);
    assert_eq!(sandboxes(&s.rt).await, Vec::<String>::new());
}

#[tokio::test]
async fn when_every_checkout_fails_nothing_is_synced() {
    let s = setup();
    *s.guest.fail_clone_of.lock().unwrap() = Some("/puddle/import/0".into());
    let report = s
        .workspaces
        .import(
            &s.rt,
            &s.id,
            &image(),
            DiskSize::gib(1),
            &[import(checkout(&s.base, "api", "main"), "api")],
        )
        .await
        .unwrap();
    assert_eq!(report.failed.len(), 1);
    assert!(!s.guest.lines().iter().any(|l| l.ends_with(" sync ")));
}

#[tokio::test]
async fn a_volume_that_exists_is_kept_and_a_failed_create_leaves_none_behind() {
    let s = setup();
    let imports = [import(checkout(&s.base, "api", "main"), "api")];
    s.rt.inject(
        Op::Create,
        Fault::once(ComputeError::Runtime {
            op: "create",
            message: "injected".into(),
        }),
    );
    let err = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::Runtime { .. }), "{err}");
    assert_eq!(s.rt.list_volumes().await.unwrap(), []);
    // A second run (the volume made by an earlier step) reports it did not make it.
    s.rt.create_volume(puddle_compute::VolumeSpec {
        name: s.id.volume_name(),
        size: DiskSize::gib(1),
    })
    .await
    .unwrap();
    let report = s
        .workspaces
        .import(&s.rt, &s.id, &image(), DiskSize::gib(1), &imports)
        .await
        .unwrap();
    assert!(!report.created_volume);
}
