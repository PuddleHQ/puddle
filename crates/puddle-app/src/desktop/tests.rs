// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_api::MemorySettings;
use serde_json::json;

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
