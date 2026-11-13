// SPDX-License-Identifier: AGPL-3.0-or-later
#[test]
fn the_fake_finds_a_checkout_by_its_head_and_its_origin_by_a_url_line() {
    let tmp = tempfile::tempdir().unwrap();
    let shop = tmp.path().join("Shop");
    std::fs::create_dir_all(shop.join(".git")).unwrap();
    std::fs::write(shop.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let service = FakeWorkspaces::new(
        Arc::new(puddle_types::NullSink),
        Arc::new(ManualClock::new(0)),
    );
    let found = service
        .local_checkout(&format!("{}/", shop.display()))
        .unwrap();
    assert_eq!(found.folder_name, "Shop");
    assert_eq!(found.git_dir, shop.join(".git"));
    assert_eq!(found.origin, Err("it has no origin remote".to_owned()));
    std::fs::write(
        shop.join(".git").join("config"),
        "[remote \"origin\"]\n\turl = https://github.com/acme/shop.git\n",
    )
    .unwrap();
    let by_git = service
        .local_checkout(&shop.join(".git").display().to_string())
        .unwrap();
    assert_eq!(
        by_git.origin.as_deref(),
        Ok("https://github.com/acme/shop.git")
    );
    let err = service
        .local_checkout(&tmp.path().display().to_string())
        .unwrap_err()
        .to_string();
    assert!(err.contains("has no .git"), "{err}");
}
use puddle_store::ManualClock;
use puddle_types::CollectingSink;

use super::*;
use crate::workspaces::{NewRepository, NoGitSetup, RepoUrl, Repository};

fn id(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn fake() -> (FakeWorkspaces, Arc<CollectingSink>) {
    let sink = Arc::new(CollectingSink::default());
    let fake = FakeWorkspaces::new(sink.clone(), Arc::new(ManualClock::new(7)));
    (fake, sink)
}

fn new_ws(name: &str) -> NewWorkspace {
    NewWorkspace::new(
        puddle_store::UserId::OWNER,
        DisplayName::parse(name).unwrap(),
        vec![NewRepository {
            repository: Repository::at_default_directory(
                EntryId::from_random([1; 5]),
                RepoUrl::parse("https://example.com/a/b").unwrap(),
            ),
            identity: None,
            pull: true,
            push: true,
            local: None,
        }],
    )
}

fn seeded(fake: &FakeWorkspaces, name: &str, status: WorkspaceStatus) {
    let mut record = WorkspaceRecord::new(id(name), DisplayName::parse(name).unwrap(), Vec::new());
    record.status = status;
    fake.seed(record, Unsaved::default());
}

fn steps(sink: &CollectingSink) -> Vec<WorkspaceStep> {
    sink.events()
        .into_iter()
        .filter_map(|e| match e {
            Event::WorkspaceProgress { step, .. } => Some(step),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_rename_changes_only_the_label_and_works_on_a_running_workspace() {
    let (fake, sink) = fake();
    seeded(&fake, "w", WorkspaceStatus::Running);
    let shown = DisplayName::parse("Web shop").unwrap();
    let renamed = fake.rename(&id("w"), shown.clone()).await.unwrap();
    assert_eq!(renamed.label(), "Web shop");
    assert_eq!(
        (renamed.id.as_str(), renamed.name.as_str()),
        ("w", "Web shop")
    );
    assert_eq!(renamed.status, WorkspaceStatus::Running);
    assert_eq!(fake.get(&id("w")).await.unwrap().name, shown);
    assert!(
        sink.events().is_empty(),
        "a rename restarts and announces nothing"
    );
}

#[tokio::test]
async fn a_rename_clash_is_refused_and_a_create_cannot_take_a_label_in_use() {
    let (fake, _) = fake();
    seeded(&fake, "a", WorkspaceStatus::Stopped);
    seeded(&fake, "b", WorkspaceStatus::Stopped);
    fake.rename(&id("a"), DisplayName::parse("Shop").unwrap())
        .await
        .unwrap();
    let clash = fake
        .rename(&id("b"), DisplayName::parse("shop").unwrap())
        .await;
    assert!(matches!(clash, Err(WorkspaceError::Conflict(_))));
    assert_eq!(fake.get(&id("b")).await.unwrap().name.as_str(), "b");
    // `a` now answers to "Shop", so a new workspace named "shop" is not free.
    let taken = fake.create(new_ws("shop"), &NoGitSetup).await;
    assert!(matches!(taken, Err(WorkspaceError::Conflict(_))));
    assert!(matches!(
        fake.rename(&id("nope"), DisplayName::parse("x").unwrap())
            .await,
        Err(WorkspaceError::NotFound(_))
    ));
}

#[tokio::test]
async fn a_failed_stop_leaves_the_workspace_running() {
    let (fake, sink) = fake();
    seeded(&fake, "w", WorkspaceStatus::Running);
    fake.fail_next(Operation::Stopping, "the VM would not stop");
    fake.stop(&id("w")).await.unwrap();
    fake.idle().await;
    assert_eq!(
        fake.get(&id("w")).await.unwrap().status,
        WorkspaceStatus::Running
    );
    assert_eq!(steps(&sink).last(), Some(&WorkspaceStep::Failed));
}

#[tokio::test]
async fn failed_reclaim_and_delete_change_nothing() {
    let (fake, sink) = fake();
    seeded(&fake, "w", WorkspaceStatus::Stopped);
    fake.fail_next(Operation::Reclaiming, "trim failed");
    fake.reclaim(&id("w")).await.unwrap();
    fake.idle().await;
    fake.fail_next(Operation::Deleting, "volume busy");
    fake.delete(&id("w"), None).await.unwrap();
    fake.idle().await;
    let record = fake.get(&id("w")).await.unwrap();
    assert_eq!(record.busy, None);
    assert_eq!(record.status, WorkspaceStatus::Stopped);
    let failed = steps(&sink)
        .iter()
        .filter(|s| **s == WorkspaceStep::Failed)
        .count();
    assert_eq!(failed, 2);
}

#[tokio::test]
async fn reclaim_waits_out_a_boot_or_shutdown() {
    let (fake, _) = fake();
    seeded(&fake, "a", WorkspaceStatus::Starting);
    seeded(&fake, "b", WorkspaceStatus::Draining);
    seeded(&fake, "c", WorkspaceStatus::Running);
    for name in ["a", "b"] {
        assert!(matches!(
            fake.reclaim(&id(name)).await,
            Err(WorkspaceError::Conflict(_))
        ));
    }
    assert!(fake.reclaim(&id("c")).await.is_ok());
    fake.idle().await;
}

#[tokio::test]
async fn names_that_make_the_same_id_get_numbers_until_the_draws_run_out() {
    let (fake, _) = fake();
    // Seventeen different names, one label: "w" and a mark.
    let names: Vec<String> = "!#$%&*+,.:;<=>?@^"
        .chars()
        .map(|m| format!("w{m}"))
        .collect();
    for (n, name) in names[..16].iter().enumerate() {
        let made = fake.create(new_ws(name), &NoGitSetup).await.unwrap();
        let want = if n == 0 {
            "w".to_owned()
        } else {
            format!("w-{}", n + 1)
        };
        assert_eq!(made.id.as_str(), want);
    }
    let err = fake
        .create(new_ws(&names[16]), &NoGitSetup)
        .await
        .unwrap_err();
    assert!(matches!(err, WorkspaceError::Unavailable(_)), "{err}");
    assert!(
        err.to_string().contains("no free workspace id was found"),
        "{err}"
    );
}

#[tokio::test]
async fn a_name_of_the_longest_length_still_makes_a_workspace() {
    let (fake, _) = fake();
    let long = "n".repeat(DisplayName::MAX_CHARS);
    let made = fake.create(new_ws(&long), &NoGitSetup).await.unwrap();
    assert_eq!(made.name.as_str(), long);
    assert!(made.id.as_str().len() <= WorkspaceId::MAX_LEN);
}

#[tokio::test]
async fn operations_on_unknown_workspaces_are_not_found() {
    let (fake, _) = fake();
    let ghost = id("ghost");
    assert!(matches!(
        fake.get(&ghost).await,
        Err(WorkspaceError::NotFound(_))
    ));
    assert!(matches!(
        fake.start(&ghost).await,
        Err(WorkspaceError::NotFound(_))
    ));
    assert!(matches!(
        fake.delete(&ghost, None).await,
        Err(WorkspaceError::NotFound(_))
    ));
    assert!(!fake.set_unsaved(&ghost, Unsaved::default()));
    assert_eq!(fake.list().await.unwrap(), []);
}

#[tokio::test]
async fn steps_pause_for_the_configured_delay() {
    tokio::time::pause();
    let sink = Arc::new(CollectingSink::default());
    let fake = FakeWorkspaces::with_options(
        sink.clone(),
        Arc::new(ManualClock::new(0)),
        Arc::new(FakeLauncher::new()),
        Duration::from_millis(300),
    );
    fake.create(new_ws("slow"), &NoGitSetup).await.unwrap();
    tokio::task::yield_now().await;
    assert!(
        steps(&sink).is_empty(),
        "nothing before the first pause ends"
    );
    fake.idle().await;
    assert_eq!(steps(&sink).len(), 4);
    assert!(format!("{fake:?}").contains("workspaces"));
}

#[tokio::test]
async fn the_fingerprint_follows_every_part_of_the_report() {
    let (fake, _) = fake();
    seeded(&fake, "w", WorkspaceStatus::Stopped);
    let base = fake.delete_check(&id("w")).await.unwrap();
    let mut seen = vec![base.fingerprint.clone()];
    let variants = [
        Unsaved {
            repos: vec![RepoFindings {
                dir: "r".into(),
                ..RepoFindings::default()
            }],
            ..Unsaved::default()
        },
        Unsaved {
            other: Listing {
                items: vec![],
                more: 1,
            },
            ..Unsaved::default()
        },
        Unsaved {
            errors: vec!["r: unreadable".into()],
            ..Unsaved::default()
        },
        Unsaved {
            repos: vec![RepoFindings {
                dir: "r".into(),
                stashes: Listing {
                    items: vec!["s".into()],
                    more: 0,
                },
                ..RepoFindings::default()
            }],
            ..Unsaved::default()
        },
    ];
    for unsaved in variants {
        assert!(fake.set_unsaved(&id("w"), unsaved));
        seen.push(fake.delete_check(&id("w")).await.unwrap().fingerprint);
    }
    let unique: std::collections::BTreeSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "{seen:?}");
}

#[tokio::test]
async fn the_launcher_records_what_it_opened() {
    let launcher = FakeLauncher::new();
    let record = WorkspaceRecord::new(id("w"), DisplayName::parse("w").unwrap(), Vec::new());
    let target = DesktopTarget {
        ssh_host: "puddle-w".into(),
        path: "/workspaces/w".into(),
        is_workspace_file: false,
    };
    launcher.open_desktop(&record, &target).await.unwrap();
    launcher.fail_next("nope");
    assert_eq!(
        launcher
            .open_desktop(&record, &target)
            .await
            .unwrap_err()
            .to_string(),
        "nope"
    );
    launcher.open_desktop(&record, &target).await.unwrap();
    assert_eq!(launcher.opened(), [id("w"), id("w")]);
}

#[tokio::test]
async fn the_launcher_records_the_browser_windows_it_opened_and_can_fail_one() {
    let launcher = FakeLauncher::new();
    let record = WorkspaceRecord::new(id("w"), DisplayName::parse("w").unwrap(), Vec::new());
    let target = BrowserTarget {
        origin: "http://w.localhost:18080".into(),
        front: SocketAddr::from(([127, 0, 0, 1], 18080)),
        settings: puddle_settings::resolve(
            &puddle_settings::GlobalSettings::default(),
            None::<&puddle_settings::WorkspaceSettings>,
        ),
        login: None,
        open: None,
    };
    launcher.fail_next("no window");
    assert_eq!(
        launcher
            .open_browser(&record, &target)
            .await
            .unwrap_err()
            .to_string(),
        "no window"
    );
    launcher.open_browser(&record, &target).await.unwrap();
    assert_eq!(launcher.browsers_opened(), [id("w")]);
    assert_eq!(launcher.opened(), []);
}

#[tokio::test]
async fn an_attach_names_the_origin_and_the_ssh_alias_by_the_id() {
    #[derive(Default)]
    struct Seen(Mutex<Vec<String>>);
    impl Launcher for Seen {
        fn open_desktop<'a>(
            &'a self,
            _workspace: &'a WorkspaceRecord,
            target: &'a DesktopTarget,
        ) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(target.ssh_host.clone());
                Ok(())
            })
        }
        fn open_browser<'a>(
            &'a self,
            _workspace: &'a WorkspaceRecord,
            target: &'a BrowserTarget,
        ) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(target.origin.clone());
                Ok(())
            })
        }
    }
    let seen = Arc::new(Seen::default());
    let fake = FakeWorkspaces::with_options(
        Arc::new(CollectingSink::default()),
        Arc::new(ManualClock::new(7)),
        seen.clone(),
        Duration::ZERO,
    );
    let mut record = WorkspaceRecord::new(
        id("k7m2x9qa"),
        DisplayName::parse("shop").unwrap(),
        Vec::new(),
    );
    record.status = WorkspaceStatus::Running;
    fake.seed(record, Unsaved::default());
    fake.attach(&id("k7m2x9qa"), AttachMode::Browser)
        .await
        .unwrap();
    fake.attach(&id("k7m2x9qa"), AttachMode::Desktop)
        .await
        .unwrap();
    let seen = seen.0.lock().unwrap().clone();
    assert!(
        seen[0].starts_with("http://k7m2x9qa.localhost:"),
        "{seen:?}"
    );
    assert_eq!(seen[1], "puddle-k7m2x9qa");
}
