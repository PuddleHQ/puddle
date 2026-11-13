// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_api::{MemorySettings, SettingsRepoError};
use serde_json::{Value, json};

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
        Err(SettingsRepoError::new(
            "/data/workspaces/a.json is not valid JSON",
        ))
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

fn a() -> WorkspaceId {
    WorkspaceId::new("a").unwrap()
}

#[test]
fn the_unreadable_repo_still_accepts_saves() {
    Unreadable.save_global(json!({})).unwrap();
    Unreadable.save_workspace(&a(), json!({})).unwrap();
    assert_eq!(Unreadable.load_app_icon().unwrap(), None);
    Unreadable.save_app_icon(b"x").unwrap();
    Unreadable.delete_app_icon().unwrap();
    Unreadable.delete_workspace(&a()).unwrap();
}

#[test]
fn a_missing_document_is_the_defaults() {
    let repo = MemorySettings::default();
    assert_eq!(global(&repo).unwrap(), GlobalSettings::default());
    assert!(workspace(&repo, &a()).is_ok());
}

#[test]
fn an_unreadable_document_names_the_file() {
    let g = global(&Unreadable).unwrap_err();
    assert!(g.contains("global settings cannot be read"), "{g}");
    assert!(g.contains("/data/global.json"), "{g}");
    let w = workspace(&Unreadable, &a()).unwrap_err();
    assert!(w.contains("workspace a cannot be read"), "{w}");
    assert!(w.contains("/data/workspaces/a.json"), "{w}");
}

#[test]
fn a_document_that_is_not_settings_is_an_error_not_the_defaults() {
    let repo = MemorySettings::default();
    repo.save_global(json!("text")).unwrap();
    repo.save_workspace(&a(), json!([1])).unwrap();
    assert!(global(&repo).unwrap_err().contains("not valid"));
    assert!(workspace(&repo, &a()).unwrap_err().contains("not valid"));
}

#[test]
fn mise_is_on_by_default_and_follows_the_workspace_switches() {
    let repo = MemorySettings::default();
    assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::On { trust: true });
    repo.save_workspace(&a(), json!({"tools": {"repo_code": false}}))
        .unwrap();
    assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::On { trust: false });
    repo.save_workspace(&a(), json!({"tools": {"mise": false}}))
        .unwrap();
    assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::Off);
}

#[test]
fn the_global_switch_wins_over_every_workspace_value() {
    let repo = MemorySettings::default();
    repo.save_global(json!({"tools": {"mise": false}})).unwrap();
    repo.save_workspace(&a(), json!({"tools": {"mise": true, "repo_code": true}}))
        .unwrap();
    assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::Off);
    // Off needs no workspace document: a damaged one does not matter.
    repo.save_workspace(&a(), json!([1])).unwrap();
    assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::Off);
}
