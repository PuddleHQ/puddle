// SPDX-License-Identifier: GPL-3.0-or-later
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

/// Makes the notices of `workspace` match `left`: the files still left get their notice, and the
/// notice of a file the hook no longer leaves (repaired, or dropped from the plan) ends. Runs
/// after every plan the hook ran in the guest, so a rewrite while the sandbox runs counts too.
pub(crate) fn settle_merge_notices(
    problems: &Problems,
    workspace: &WorkspaceId,
    left: &[(&str, &str)],
) {
    let notices = merge_notices(workspace, left);
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
    fn each_file_has_its_own_consequence() {
        let left = [
            ("/root/.docker/config.json", "x"),
            (MACHINE_SETTINGS_GUEST, "x"),
            (SSH_CONFIG_SYSTEM, "x"),
        ];
        let details: Vec<String> = merge_notices(&workspace(), &left)
            .into_iter()
            .map(|p| p.detail)
            .collect();
        assert!(details[0].contains("Docker does not get Puddle's proxy settings"));
        assert!(details[1].contains("Git sign-in guard is not switched on"));
        assert!(details[2].contains("\"Connection closed\""));
        assert!(details[2].contains("git, scp and rsync"));
    }

    #[test]
    fn a_docker_config_in_another_folder_still_says_pulls_fail() {
        let left = [("/work/docker/config.json", "x")];
        let [problem] = merge_notices(&workspace(), &left).try_into().unwrap();
        assert!(problem.detail.contains("bypasses the proxy"));
    }

    fn settled(left: &[(&str, &str)], problems: &Problems) {
        settle_merge_notices(problems, &workspace(), left);
    }

    fn keys(problems: &Problems) -> Vec<String> {
        problems.list().into_iter().map(|p| p.key).collect()
    }

    #[test]
    fn settling_raises_the_left_files_and_ends_the_ones_no_longer_left() {
        let problems = Problems::new(std::sync::Arc::new(puddle_types::CollectingSink::default()));
        settled(&[("/a", "bad"), ("/b", "bad")], &problems);
        assert_eq!(
            keys(&problems),
            ["merge-left:acme:/a", "merge-left:acme:/b"]
        );
        settled(&[("/b", "still bad")], &problems);
        assert_eq!(keys(&problems), ["merge-left:acme:/b"]);
        assert!(problems.list()[0].detail.contains("still bad"));
        settled(&[], &problems);
        assert_eq!(problems.list(), Vec::<Problem>::new());
    }

    #[test]
    fn settling_leaves_other_problems_and_other_workspaces_alone() {
        let problems = Problems::new(std::sync::Arc::new(puddle_types::CollectingSink::default()));
        problems.raise(Problem::new("kubernetes:acme", "t", "d"));
        problems.raise(Problem::new("merge-left:acmeless:/a", "t", "d"));
        settled(&[], &problems);
        assert_eq!(
            keys(&problems),
            ["kubernetes:acme", "merge-left:acmeless:/a"]
        );
    }

    #[test]
    fn the_editors_own_file_has_its_own_notice() {
        let left = [("/work/.puddle/w.code-workspace", "bad")];
        assert_eq!(merge_notices(&workspace(), &left), Vec::<Problem>::new());
    }
}
