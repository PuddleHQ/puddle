// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Mutex;

use puddle_api::{MemorySettings, SettingsRepo};
use puddle_secrets::MemoryStore;
use serde_json::json;

use super::*;

#[derive(Default)]
struct Events(Mutex<Vec<Event>>);

impl EventSink for Events {
    fn emit(&self, event: Event) {
        self.0.lock().unwrap().push(event);
    }
}

fn workspace(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[test]
fn each_problem_becomes_the_event_that_names_it_with_the_service_as_the_user_knows_it() {
    let events = Arc::new(Events::default());
    let notices = EventNotices {
        events: events.clone(),
    };
    let all = [
        (
            Problem::StoreUnavailable,
            LoginProblemKind::StoreUnavailable,
        ),
        (
            Problem::UnexpectedAnswer,
            LoginProblemKind::UnexpectedAnswer,
        ),
        (Problem::UnusableToken, LoginProblemKind::UnusableToken),
        (Problem::BoundToken, LoginProblemKind::BoundToken),
        (Problem::UnnamedToken, LoginProblemKind::UnnamedToken),
        (Problem::Unreadable, LoginProblemKind::Unreadable),
        (Problem::CaptureOff, LoginProblemKind::CaptureOff),
    ];
    for (problem, _) in all {
        notices.problem(&workspace("alpha"), &builtin()[0], problem);
    }
    let sent = events.0.lock().unwrap();
    for ((_, kind), event) in all.iter().zip(sent.iter()) {
        assert_eq!(
            *event,
            Event::LoginProblem {
                workspace: workspace("alpha"),
                service: "Claude Code".into(),
                kind: *kind,
            }
        );
    }
    assert_eq!(sent.len(), all.len());
}

#[tokio::test]
async fn a_workspace_gets_logins_over_its_own_registry_and_capture_decides_what_it_decrypts() {
    let store = Arc::new(MemoryStore::new());
    let events = Arc::new(Events::default());
    let keeper = LoginKeeper::new(store, events.clone());
    let registry = Arc::new(StandIns::new());
    let on = keeper.logins(&workspace("alpha"), true, Arc::clone(&registry));
    assert!(!on.hosts().is_empty());
    let off = keeper.logins(&workspace("alpha"), false, registry);
    assert!(off.hosts().is_empty());
    // Nothing is kept, so loading says nothing.
    on.load().await;
    assert!(events.0.lock().unwrap().is_empty());
}

/// The log lines written while `run` polled on this thread.
async fn logged<F: Future<Output = ()>>(run: F) -> String {
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    run.await;
    std::io::Write::flush(&mut capture.clone()).unwrap();
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[tokio::test]
async fn deleting_a_workspace_logs_the_tokens_a_refusing_credential_store_kept_and_is_quiet_otherwise()
 {
    let store = Arc::new(MemoryStore::new());
    let keeper = LoginKeeper::new(store.clone(), Arc::new(Events::default()));
    let quiet = logged(keeper.delete_workspace(&workspace("alpha"))).await;
    assert_eq!(quiet, "");

    store.break_it();
    let warned = logged(keeper.delete_workspace(&workspace("alpha"))).await;
    assert!(warned.contains("WARN"), "{warned}");
    assert!(warned.contains("workspace=alpha"), "{warned}");
    assert!(warned.contains("could not be removed"), "{warned}");
    assert!(!warned.contains("failed=0"), "{warned}");
}

#[test]
fn capture_is_on_unless_the_workspace_or_the_global_setting_turns_it_off() {
    let repo = MemorySettings::default();
    let ws = workspace("alpha");
    assert!(capture_enabled(&repo, &ws));
    repo.save_global(json!({"workspace_defaults": {"capture_logins": false}}))
        .unwrap();
    assert!(!capture_enabled(&repo, &ws));
    repo.save_workspace(&ws, json!({"overrides": {"capture_logins": true}}))
        .unwrap();
    assert!(capture_enabled(&repo, &ws));
    assert!(!capture_enabled(&repo, &workspace("beta")));
}

#[test]
fn the_foreign_credential_warning_is_on_unless_the_workspace_or_the_global_setting_turns_it_off() {
    let repo = MemorySettings::default();
    let ws = workspace("alpha");
    assert!(foreign_warning_enabled(&repo, &ws));
    repo.save_global(json!({"workspace_defaults": {"warn_foreign_credentials": false}}))
        .unwrap();
    assert!(!foreign_warning_enabled(&repo, &ws));
    repo.save_workspace(
        &ws,
        json!({"overrides": {"warn_foreign_credentials": true}}),
    )
    .unwrap();
    assert!(foreign_warning_enabled(&repo, &ws));
    assert!(!foreign_warning_enabled(&repo, &workspace("beta")));
}

#[test]
fn settings_that_cannot_be_read_leave_the_foreign_credential_warning_on() {
    let repo = MemorySettings::default();
    repo.save_global(json!("not settings")).unwrap();
    assert!(foreign_warning_enabled(&repo, &workspace("alpha")));
}

#[test]
fn settings_that_cannot_be_read_leave_capture_on() {
    let repo = MemorySettings::default();
    repo.save_global(json!("not settings")).unwrap();
    assert!(capture_enabled(&repo, &workspace("alpha")));
}
