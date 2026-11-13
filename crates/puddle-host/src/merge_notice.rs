// SPDX-License-Identifier: AGPL-3.0-or-later
//! The notice for a file of the user's that Puddle merges its settings into and cannot parse:
//! the file stays as it is, so the settings Puddle wanted in it are missing until it is fixed.

use puddle_boot::MACHINE_SETTINGS_GUEST;
use puddle_types::{Problem, Problems, WorkspaceId};

/// The system `ssh_config`, where Puddle adds one `Include` line.
const SSH_CONFIG_SYSTEM: &str = "/etc/ssh/ssh_config";

/// What the editor's multi-root file is called; its own notice covers it (the attach that writes
/// it).
const EDITOR_FILE_SUFFIX: &str = ".code-workspace";

/// The start of every key of `workspace`'s merge notices, for ending them all.
pub(crate) fn merge_notice_prefix(workspace: &WorkspaceId) -> String {
    format!("merge-left:{workspace}:")
}

/// What does not work while `path` is left unmerged.
fn consequence(path: &str) -> &'static str {
    if path == MACHINE_SETTINGS_GUEST {
        "VS Code in this workspace keeps its own port forwarding, so it can forward the port of \
         Puddle's proxy, and its Git sign-in guard is not switched on"
    } else if path == SSH_CONFIG_SYSTEM {
        "ssh in this workspace, and git, scp and rsync over it, do not read Puddle's SSH \
         settings, so their connections fail with a bare \"Connection closed\" instead of \
         Puddle's message"
    } else if path.ends_with("/config.json") {
        "Docker does not get Puddle's proxy settings and bypasses the proxy, so image pulls and \
         builds that need the network fail"
    } else {
        "Puddle's settings for it are not applied"
    }
}

/// The notices for the files the boot hook left as they were (`left`: each path with the reason
/// the hook gave), one per file. The file editor's multi-root file is not one of them.
pub(crate) fn merge_notices(
    workspace: &WorkspaceId,
    name: &str,
    left: &[(&str, &str)],
) -> Vec<Problem> {
    left.iter()
        .filter(|(path, _)| !path.ends_with(EDITOR_FILE_SUFFIX))
        .map(|(path, why)| {
            Problem::new(
                format!("{}{path}", merge_notice_prefix(workspace)),
                format!("Puddle could not update {path} in {name}"),
                format!(
                    "{path} was left as it is: {why}. Until it is fixed, {}. Puddle checks it \
                     again at the next start.",
                    consequence(path)
                ),
            )
            .about(workspace.clone())
        })
        .collect()
}

/// Makes the notices of `workspace` match `left`: the files still left get their notice, and the
/// notice of a file the hook no longer leaves (repaired, or dropped from the plan) ends. Runs
/// after every plan the hook ran in the guest, so a rewrite while the sandbox runs counts too.
pub(crate) fn settle_merge_notices(
    problems: &Problems,
    workspace: &WorkspaceId,
    name: &str,
    left: &[(&str, &str)],
) {
    let notices = merge_notices(workspace, name, left);
    let prefix = merge_notice_prefix(workspace);
    for stale in problems.list() {
        if stale.key.starts_with(&prefix) && !notices.iter().any(|n| n.key == stale.key) {
            problems.clear(&stale.key);
        }
    }
    for notice in notices {
        problems.raise(notice);
    }
}

#[cfg(test)]
mod tests;
