// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fixtures of every module in this binary must not touch the repository a git hook points
//! `GIT_DIR` at.

use crate::common;

/// `git init --bare` under a hook's `GIT_DIR` turns that repository bare. One run of the whole
/// binary covers every module's fixtures, so this lives here and not in each module.
#[test]
fn fixtures_leave_the_repository_of_a_git_hook_alone() {
    // skip-ok: the parent run starts this binary again as a child, and the child must not recurse.
    if common::is_hook_env_child() {
        return;
    }
    common::assert_hook_env_leaves_the_repository_alone(
        "fixtures_leave_the_repository_of_a_git_hook_alone",
    );
}
