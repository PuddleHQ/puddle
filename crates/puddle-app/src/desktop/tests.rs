// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_api::MemorySettings;
use serde_json::json;

use std::sync::atomic::AtomicUsize;

use super::*;

#[test]
fn a_notification_opens_the_inbox_at_its_request_or_the_inbox_as_it_is() {
    let base = Url::parse("http://127.0.0.1:4000/settings?x=1#top").unwrap();
    assert_eq!(
        inbox_url(&base, Opens::Request(42)).as_str(),
        "http://127.0.0.1:4000/inbox?request=42#top"
    );
    assert_eq!(
        inbox_url(&base, Opens::Inbox).as_str(),
        "http://127.0.0.1:4000/inbox#top"
    );
}

#[test]
fn the_preferences_come_from_the_global_settings_and_default_without_any() {
    let repo = MemorySettings::default();
    assert_eq!(read_prefs(&repo).unwrap(), UiPrefs::default());
    repo.save_global(
        json!({"schema_version": 2, "ui": {"close_behaviour": "quit", "minimize_to_tray": true}}),
    )
    .unwrap();
    let prefs = read_prefs(&repo).unwrap();
    assert_eq!(
        prefs.close_behaviour(),
        puddle_settings::CloseBehaviour::Quit
    );
    assert!(prefs.minimize_to_tray());
}

#[test]
fn settings_that_are_not_valid_say_so_and_the_window_still_gets_defaults() {
    let repo = MemorySettings::default();
    repo.save_global(json!({"ui": {"close_behaviour": "explode"}}))
        .unwrap();
    let reason = read_prefs(&repo).unwrap_err();
    assert!(reason.starts_with("the settings are not valid"), "{reason}");
    assert_eq!(prefs_or_default(&repo), UiPrefs::default());
}

#[test]
fn the_tray_notice_is_once_per_data_folder_and_once_per_run_without_one() {
    let folder = tempfile::tempdir().unwrap();
    let run = AtomicBool::new(false);
    assert!(first_time(Some(folder.path()), &run));
    assert!(!first_time(Some(folder.path()), &run));
    // A new run, same folder: still told.
    assert!(!first_time(Some(folder.path()), &AtomicBool::new(false)));

    let without = AtomicBool::new(false);
    assert!(first_time(None, &without));
    assert!(!first_time(None, &without));
    // A folder that cannot be written falls back to this run.
    let missing = folder.path().join("gone");
    let fallback = AtomicBool::new(false);
    assert!(first_time(Some(&missing), &fallback));
    assert!(!first_time(Some(&missing), &fallback));
}

#[test]
fn the_quit_question_names_the_open_terminals_and_is_none_without_any() {
    assert_eq!(quit_question(0), None);
    assert_eq!(
        quit_question(1).as_deref(),
        Some("Quit Puddle? This stops every workspace and ends 1 open terminal.")
    );
    assert_eq!(
        quit_question(3).as_deref(),
        Some("Quit Puddle? This stops every workspace and ends 3 open terminals.")
    );
}

/// Presses Ctrl+Q with `open` terminals; returns what was asked and how often it quit.
fn press(
    open: usize,
    asking: &Arc<AtomicBool>,
    quits: &Arc<AtomicUsize>,
) -> (Option<String>, Option<QuitAnswer>) {
    let asked = std::cell::RefCell::new(None);
    let counter = Arc::clone(quits);
    on_quit_pressed(
        open,
        asking,
        |question, answer| *asked.borrow_mut() = Some((question, answer)),
        move || {
            counter.fetch_add(1, Ordering::SeqCst);
        },
    );
    match asked.into_inner() {
        Some((question, answer)) => (Some(question), Some(answer)),
        None => (None, None),
    }
}

#[test]
fn ctrl_q_quits_at_once_when_no_terminal_is_open() {
    let (asking, quits) = (Arc::default(), Arc::default());
    let (question, _) = press(0, &asking, &quits);
    assert_eq!(question, None);
    assert_eq!(quits.load(Ordering::SeqCst), 1);
}

#[test]
fn ctrl_q_asks_while_a_terminal_is_open_and_quits_only_on_yes() {
    let (asking, quits) = (Arc::default(), Arc::default());
    let (question, answer) = press(2, &asking, &quits);
    assert_eq!(
        question.as_deref(),
        Some("Quit Puddle? This stops every workspace and ends 2 open terminals.")
    );
    assert_eq!(
        quits.load(Ordering::SeqCst),
        0,
        "nothing quits before the answer"
    );

    // Cancel: no quit, and the next press asks again.
    answer.unwrap()(false);
    assert_eq!(quits.load(Ordering::SeqCst), 0);
    let (_, answer) = press(2, &asking, &quits);
    assert!(answer.is_some(), "the shortcut asks again after a cancel");
    answer.unwrap()(true);
    assert_eq!(quits.load(Ordering::SeqCst), 1);
}

#[test]
fn ctrl_q_pressed_again_while_the_question_is_up_asks_nothing_more() {
    let (asking, quits) = (Arc::default(), Arc::default());
    let (_, answer) = press(1, &asking, &quits);
    let (second, second_answer) = press(1, &asking, &quits);
    assert_eq!((second, second_answer.is_none()), (None, true));
    assert_eq!(quits.load(Ordering::SeqCst), 0);
    answer.unwrap()(false);
}
