// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn only_git_variables_are_removed() {
    let cmd = command_without(
        "git",
        ["GIT_DIR", "GIT_INDEX_FILE", "PATH", "GIT_CONFIG_COUNT"].map(OsString::from),
    );
    let mut removed: Vec<_> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    removed.sort();
    assert_eq!(removed, ["GIT_CONFIG_COUNT", "GIT_DIR", "GIT_INDEX_FILE"]);
}
