// SPDX-License-Identifier: GPL-3.0-or-later
//! The notice for a file of the user's that Puddle merges its settings into and cannot parse:
//! the file stays as it is, so the settings Puddle wanted in it are missing until it is fixed.

use puddle_boot::MACHINE_SETTINGS_GUEST;
use puddle_types::{Problem, WorkspaceId};

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
        "ssh in this workspace does not read Puddle's SSH settings, so its connections do not go \
         through Puddle"
    } else if path.ends_with("/config.json") {
        "Docker does not get Puddle's proxy settings and bypasses the proxy, so image pulls and \
         builds that need the network fail"
    } else {
        "Puddle's settings for it are not applied"
    }
}

/// The notices for the files the boot hook left as they were (`left`: each path with the reason
/// the hook gave), one per file. The file editor's multi-root file is not one of them.
pub(crate) fn merge_notices(workspace: &WorkspaceId, left: &[(&str, &str)]) -> Vec<Problem> {
    left.iter()
        .filter(|(path, _)| !path.ends_with(EDITOR_FILE_SUFFIX))
        .map(|(path, why)| {
            Problem::new(
                format!("{}{path}", merge_notice_prefix(workspace)),
                format!("Puddle could not update {path} in {workspace}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> WorkspaceId {
        WorkspaceId::new("acme").unwrap()
    }

    #[test]
    fn no_left_file_is_no_notice() {
        assert_eq!(merge_notices(&workspace(), &[]), Vec::<Problem>::new());
    }

    #[test]
    fn docker_config_says_pulls_fail() {
        let left = [(
            "/root/.docker/config.json",
            "the file is not valid JSON: EOF",
        )];
        let [problem] = merge_notices(&workspace(), &left).try_into().unwrap();
        assert_eq!(problem.key, "merge-left:acme:/root/.docker/config.json");
        assert_eq!(problem.workspace, Some(workspace()));
        assert_eq!(
            problem.title,
            "Puddle could not update /root/.docker/config.json in acme"
        );
        assert!(
            problem.detail.contains("not valid JSON: EOF")
                && problem.detail.contains("bypasses the proxy")
                && problem.detail.contains("pulls"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn machine_settings_say_what_the_editor_loses() {
        let left = [(MACHINE_SETTINGS_GUEST, "the file is not valid JSON")];
        let [problem] = merge_notices(&workspace(), &left).try_into().unwrap();
        assert!(
            problem.detail.contains("port forwarding"),
            "{}",
            problem.detail
        );
    }

    #[test]
    fn ssh_config_and_unknown_files_have_their_own_words() {
        let left = [(SSH_CONFIG_SYSTEM, "x"), ("/root/other", "y")];
        let notices = merge_notices(&workspace(), &left);
        assert!(notices[0].detail.contains("ssh in this workspace"));
        assert!(
            notices[1]
                .detail
                .contains("Puddle's settings for it are not applied")
        );
    }

    #[test]
    fn the_editors_own_file_has_its_own_notice() {
        let left = [("/work/.puddle/w.code-workspace", "bad")];
        assert_eq!(merge_notices(&workspace(), &left), Vec::<Problem>::new());
    }
}
