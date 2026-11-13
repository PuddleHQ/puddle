// SPDX-License-Identifier: GPL-3.0-or-later
//! Helpers for other crates' tests (the `testing` feature; never in a shipped program).
#![expect(clippy::panic, reason = "a test helper fails the test by panicking")]

use puddle_types::WorkspaceId;

use crate::store::{AllUsers, Store};
use crate::users::UserId;

/// Records the owner as the user of `workspace`, as creating it as the owner does: the one way a
/// test sets up a workspace it never creates. A workspace with no user recorded is nobody's, and
/// the store refuses to decide, read or change anything for it.
///
/// # Panics
/// When the workspace is already another user's, or the database fails.
pub fn own(store: &Store, workspace: &WorkspaceId) {
    store
        .set_workspace_owner(workspace, UserId::OWNER)
        .unwrap_or_else(|err| panic!("recording the owner of {workspace}: {err}"));
}

/// The reach across every user's data that the host alone holds in a running install (see
/// [`Store::take_all_users`]), for a test that checks what the install sees.
#[must_use]
pub fn all_users() -> AllUsers {
    AllUsers::new()
}
