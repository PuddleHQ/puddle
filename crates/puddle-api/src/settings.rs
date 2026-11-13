// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the API keeps settings documents. The API reads and writes raw documents and does the
//! versioning itself (`puddle-settings`: migrate, keep unknown fields, refuse newer), so a store
//! only has to keep JSON values.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use puddle_types::WorkspaceId;
use serde_json::Value;

/// Storage for the two settings document kinds. Calls may block (files, `SQLite`); the API runs
/// them on a blocking thread.
pub trait SettingsRepo: Send + Sync {
    /// The global document, or `None` if none was ever written.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn load_global(&self) -> Result<Option<Value>, SettingsRepoError>;

    /// Replaces the global document.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn save_global(&self, document: Value) -> Result<(), SettingsRepoError>;

    /// One workspace's document, or `None` if it has none.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn load_workspace(&self, workspace: &WorkspaceId) -> Result<Option<Value>, SettingsRepoError>;

    /// Replaces one workspace's document.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn save_workspace(
        &self,
        workspace: &WorkspaceId,
        document: Value,
    ) -> Result<(), SettingsRepoError>;

    /// Removes one workspace's document (and a copy under the old folder name). A workspace
    /// with none is not an error.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if a document exists and cannot be removed.
    fn delete_workspace(&self, workspace: &WorkspaceId) -> Result<(), SettingsRepoError>;

    /// The user's own app icon (a PNG made by `puddle-appicon`), or `None` if none was given.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn load_app_icon(&self) -> Result<Option<Vec<u8>>, SettingsRepoError>;

    /// Replaces the user's own app icon.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn save_app_icon(&self, png: &[u8]) -> Result<(), SettingsRepoError>;

    /// Removes the user's own app icon; removing none is not an error.
    ///
    /// # Errors
    ///
    /// [`SettingsRepoError`] if the storage fails.
    fn delete_app_icon(&self) -> Result<(), SettingsRepoError>;
}

/// A settings storage failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("settings storage failed: {reason}")]
pub struct SettingsRepoError {
    reason: String,
}

impl SettingsRepoError {
    /// An error with this reason (no secrets: it is logged).
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

/// Settings kept in memory, for tests and until the storage row lands.
#[derive(Debug, Default)]
pub struct MemorySettings {
    global: Mutex<Option<Value>>,
    workspaces: Mutex<BTreeMap<WorkspaceId, Value>>,
    app_icon: Mutex<Option<Vec<u8>>>,
}

impl SettingsRepo for MemorySettings {
    fn load_global(&self) -> Result<Option<Value>, SettingsRepoError> {
        Ok(self
            .global
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn save_global(&self, document: Value) -> Result<(), SettingsRepoError> {
        *self.global.lock().unwrap_or_else(PoisonError::into_inner) = Some(document);
        Ok(())
    }

    fn load_workspace(&self, workspace: &WorkspaceId) -> Result<Option<Value>, SettingsRepoError> {
        Ok(self
            .workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .cloned())
    }

    fn save_workspace(
        &self,
        workspace: &WorkspaceId,
        document: Value,
    ) -> Result<(), SettingsRepoError> {
        self.workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(workspace.clone(), document);
        Ok(())
    }

    fn delete_workspace(&self, workspace: &WorkspaceId) -> Result<(), SettingsRepoError> {
        self.workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(workspace);
        Ok(())
    }

    fn load_app_icon(&self) -> Result<Option<Vec<u8>>, SettingsRepoError> {
        Ok(self
            .app_icon
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    fn save_app_icon(&self, png: &[u8]) -> Result<(), SettingsRepoError> {
        *self.app_icon.lock().unwrap_or_else(PoisonError::into_inner) = Some(png.to_vec());
        Ok(())
    }

    fn delete_app_icon(&self) -> Result<(), SettingsRepoError> {
        *self.app_icon.lock().unwrap_or_else(PoisonError::into_inner) = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
