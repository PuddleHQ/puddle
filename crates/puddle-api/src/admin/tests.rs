// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_workspace_is_up_while_its_sandbox_runs_or_is_coming_or_going() {
    for up in [
        WorkspaceStatus::Starting,
        WorkspaceStatus::Running,
        WorkspaceStatus::Draining,
        WorkspaceStatus::Paused,
    ] {
        assert!(is_up(up), "{up:?}");
    }
    for down in [
        WorkspaceStatus::Created,
        WorkspaceStatus::Stopped,
        WorkspaceStatus::Crashed,
        WorkspaceStatus::VolumeMissing,
    ] {
        assert!(!is_up(down), "{down:?}");
    }
}

#[test]
fn counts_saturate_instead_of_wrapping() {
    assert_eq!(count(3), 3);
    assert_eq!(count(usize::MAX), u32::MAX);
}
