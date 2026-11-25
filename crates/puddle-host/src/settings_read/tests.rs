// SPDX-License-Identifier: AGPL-3.0-or-later
use futures_util::future::BoxFuture;
use puddle_api::{MemorySettings, SettingsLock, SettingsRepoError};
use serde_json::{Value, json};
use tokio::sync::OwnedMutexGuard;

use super::*;

/// A repo whose reads fail, like a file that cannot be read.
struct Unreadable;

impl SettingsRepo for Unreadable {
    /// Nothing changes these documents, so the cycles it would serialise do not exist.
    fn lock(&self) -> BoxFuture<'static, OwnedMutexGuard<()>> {
        SettingsLock::default().hold()
    }
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
    let g = global(&Unreadable).unwrap_err().to_string();
    assert!(g.contains("global settings cannot be read"), "{g}");
    assert!(g.contains("/data/global.json"), "{g}");
    let w = workspace(&Unreadable, &a()).unwrap_err().to_string();
    assert!(w.contains("workspace a cannot be read"), "{w}");
    assert!(w.contains("/data/workspaces/a.json"), "{w}");
}

#[test]
fn a_document_that_is_not_settings_is_an_error_not_the_defaults() {
    let repo = MemorySettings::default();
    repo.save_global(json!("text")).unwrap();
    repo.save_workspace(&a(), json!([1])).unwrap();
    assert!(global(&repo).unwrap_err().to_string().contains("not valid"));
    assert!(
        workspace(&repo, &a())
            .unwrap_err()
            .to_string()
            .contains("not valid")
    );
}

#[test]
fn mise_is_on_by_default_and_follows_the_workspace_switches() {
    let repo = MemorySettings::default();
    if !puddle_settings::Tools::AVAILABLE {
        // The default build leaves the layer out (dormant): off whatever is stored.
        assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::Off);
        repo.save_workspace(&a(), json!({"tools": {"mise": true, "repo_code": true}}))
            .unwrap();
        assert_eq!(mise(&repo, &a()).unwrap(), MiseMode::Off);
        return;
    }
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
