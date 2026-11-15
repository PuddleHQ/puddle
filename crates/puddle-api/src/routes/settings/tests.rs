// SPDX-License-Identifier: AGPL-3.0-or-later
use crate::error::ErrorCode;
use crate::settings::SettingsRepoError;

use super::*;

/// A repo whose reads fail, like a file that cannot be read.
struct Unreadable;

impl SettingsRepo for Unreadable {
    fn load_global(&self) -> Result<Option<Value>, SettingsRepoError> {
        Err(SettingsRepoError::new(
            "/data/global.json is not valid JSON",
        ))
    }
    fn save_global(&self, _: Value) -> Result<(), SettingsRepoError> {
        Ok(())
    }
    fn load_workspace(&self, _: &WorkspaceId) -> Result<Option<Value>, SettingsRepoError> {
        Err(SettingsRepoError::new("permission denied"))
    }
    fn save_workspace(&self, _: &WorkspaceId, _: Value) -> Result<(), SettingsRepoError> {
        Ok(())
    }
    fn delete_workspace(&self, _: &WorkspaceId) -> Result<(), SettingsRepoError> {
        Ok(())
    }
    fn load_app_icon(&self) -> Result<Option<Vec<u8>>, SettingsRepoError> {
        Ok(None)
    }
    fn save_app_icon(&self, _: &[u8]) -> Result<(), SettingsRepoError> {
        Ok(())
    }
    fn delete_app_icon(&self) -> Result<(), SettingsRepoError> {
        Ok(())
    }
}

#[test]
fn the_unreadable_repo_still_accepts_saves() {
    Unreadable.save_global(json!({})).unwrap();
    let name = WorkspaceId::new("box").unwrap();
    Unreadable.save_workspace(&name, json!({})).unwrap();
    assert_eq!(Unreadable.load_app_icon().unwrap(), None);
    Unreadable.save_app_icon(b"x").unwrap();
    Unreadable.delete_app_icon().unwrap();
    Unreadable.delete_workspace(&name).unwrap();
}

#[test]
fn a_repo_that_cannot_read_says_so_with_the_cause() {
    let err = load_global(&Unreadable).unwrap_err();
    assert_eq!(err.code(), ErrorCode::SettingsUnreadable);
    let message = err.message();
    assert!(message.contains("the global settings"));
    assert!(message.contains("/data/global.json is not valid JSON"));
    // The bare cause is what other reports (the workspace record) wrap in their own words.
    let reason = err.reason();
    assert!(!reason.contains("Puddle cannot read"));

    let name = WorkspaceId::new("box").unwrap();
    let err = load_workspace(&Unreadable, &name).unwrap_err();
    assert_eq!(err.code(), ErrorCode::SettingsUnreadable);
    let message = err.message();
    assert!(message.contains("workspace box"));
    assert!(message.contains("permission denied"));
}
