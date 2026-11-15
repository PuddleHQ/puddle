// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

#[test]
fn memory_settings_keep_what_was_saved() {
    let repo = MemorySettings::default();
    let a = WorkspaceId::new("a").unwrap();
    assert_eq!(repo.load_global().unwrap(), None);
    assert_eq!(repo.load_workspace(&a).unwrap(), None);
    repo.save_global(json!({"schema_version": 1})).unwrap();
    repo.save_workspace(&a, json!({"overrides": {}})).unwrap();
    assert_eq!(
        repo.load_global().unwrap(),
        Some(json!({"schema_version": 1}))
    );
    assert_eq!(
        repo.load_workspace(&a).unwrap(),
        Some(json!({"overrides": {}}))
    );
    let b = WorkspaceId::new("b").unwrap();
    repo.save_workspace(&b, json!({"overrides": {"cpus": 2}}))
        .unwrap();
    repo.delete_workspace(&a).unwrap();
    repo.delete_workspace(&a).unwrap();
    assert_eq!(repo.load_workspace(&a).unwrap(), None);
    assert!(repo.load_workspace(&b).unwrap().is_some());
    assert_eq!(repo.load_app_icon().unwrap(), None);
    repo.save_app_icon(b"png").unwrap();
    assert_eq!(repo.load_app_icon().unwrap(), Some(b"png".to_vec()));
    repo.delete_app_icon().unwrap();
    assert_eq!(repo.load_app_icon().unwrap(), None);
    assert_eq!(
        SettingsRepoError::new("disk full").to_string(),
        "settings storage failed: disk full"
    );
}
