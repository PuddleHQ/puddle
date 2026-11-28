// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace that is known to belong to the user who asks: the argument a service method takes
//! in place of a bare [`WorkspaceId`].
//!
//! The route table (`puddle-api`'s `authz`) already refuses another
//! user's workspace before a handler runs, but a service method that takes only an id cannot tell
//! whose call it is, so a handler that forgets the check, or a second caller, reaches every
//! user's workspaces. An [`OwnedWorkspace`] can only be made by [`OwnedWorkspace::resolve`], which
//! asks who owns the workspace, so a method that takes one is scoped by its signature.

use puddle_store::{Store, StoreError, UserId};
use puddle_types::WorkspaceId;

use crate::workspaces::WorkspaceError;

/// Who owns a workspace. The store is the one real answer; a test can give another.
pub trait WorkspaceOwnership {
    /// The user the workspace belongs to; `None` when it is nobody's (unknown id).
    ///
    /// # Errors
    /// The owner could not be looked up.
    fn owner_of(&self, workspace: &WorkspaceId) -> Result<Option<UserId>, StoreError>;
}

impl WorkspaceOwnership for Store {
    fn owner_of(&self, workspace: &WorkspaceId) -> Result<Option<UserId>, StoreError> {
        self.workspace_owner(workspace)
    }
}

/// The workspace is not the user's, or does not exist: one answer for both, so another user's
/// workspace cannot be told from an unknown id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no such workspace")]
pub struct NotOwned;

/// A workspace and the user it belongs to, proven together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedWorkspace {
    id: WorkspaceId,
    user: UserId,
}

impl OwnedWorkspace {
    /// The handle for `workspace` if it belongs to `user`.
    ///
    /// # Errors
    /// [`ResolveError::NotOwned`] for a workspace that is another user's or unknown,
    /// [`ResolveError::Lookup`] when the owner could not be read.
    pub fn resolve(
        owners: &impl WorkspaceOwnership,
        user: UserId,
        workspace: &WorkspaceId,
    ) -> Result<Self, ResolveError> {
        match owners.owner_of(workspace).map_err(ResolveError::Lookup)? {
            Some(owner) if owner == user => Ok(Self {
                id: workspace.clone(),
                user,
            }),
            _ => Err(ResolveError::NotOwned(NotOwned)),
        }
    }

    /// The workspace.
    #[must_use]
    pub fn id(&self) -> &WorkspaceId {
        &self.id
    }

    /// The user it belongs to, who is also the user asking.
    #[must_use]
    pub fn user(&self) -> UserId {
        self.user
    }
}

/// Why no handle could be made.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// Another user's workspace, or none.
    #[error(transparent)]
    NotOwned(NotOwned),
    /// The store could not say who owns it.
    #[error("the owner of the workspace could not be read: {0}")]
    Lookup(StoreError),
}

impl From<ResolveError> for WorkspaceError {
    fn from(err: ResolveError) -> Self {
        match err {
            ResolveError::NotOwned(_) => Self::NotFound("no such workspace".into()),
            ResolveError::Lookup(err) => Self::internal(err),
        }
    }
}

#[cfg(test)]
mod tests;
