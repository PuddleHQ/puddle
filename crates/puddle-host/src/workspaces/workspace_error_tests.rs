// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_workspace_held_by_a_sandbox_is_a_conflict_a_missing_one_not_found_and_the_rest_internal() {
    let held = puddle_workspace::WorkspaceError::InUse {
        workspace: "a0k3x9zq".to_owned(),
        holder: "shop".to_owned(),
    };
    assert_eq!(
        workspace_error(&held),
        WorkspaceError::Conflict(held.to_string())
    );
    let gone = puddle_workspace::WorkspaceError::NotFound {
        workspace: "a0k3x9zq".to_owned(),
    };
    assert_eq!(
        workspace_error(&gone),
        WorkspaceError::NotFound(gone.to_string())
    );
    let other = puddle_workspace::WorkspaceError::Layout {
        reason: "boom".to_owned(),
    };
    assert_eq!(
        workspace_error(&other),
        WorkspaceError::Internal(other.to_string())
    );
}
