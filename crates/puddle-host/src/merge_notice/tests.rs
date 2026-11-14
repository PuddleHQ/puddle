// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn workspace() -> WorkspaceId {
    WorkspaceId::new("acme").unwrap()
}

#[test]
fn no_left_file_is_no_notice() {
    assert_eq!(
        merge_notices(&workspace(), "Acme shop", &[]),
        Vec::<Problem>::new()
    );
}

#[test]
fn docker_config_says_pulls_fail() {
    let left = [(
        "/root/.docker/config.json",
        "the file is not valid JSON: EOF",
    )];
    let [problem] = merge_notices(&workspace(), "Acme shop", &left)
        .try_into()
        .unwrap();
    assert_eq!(problem.key, "merge-left:acme:/root/.docker/config.json");
    assert_eq!(problem.workspace, Some(workspace()));
    assert_eq!(
        problem.title,
        "Puddle could not update /root/.docker/config.json in Acme shop"
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
    let [problem] = merge_notices(&workspace(), "Acme shop", &left)
        .try_into()
        .unwrap();
    assert!(
        problem.detail.contains("port forwarding"),
        "{}",
        problem.detail
    );
}

#[test]
fn ssh_config_and_unknown_files_have_their_own_words() {
    let left = [(SSH_CONFIG_SYSTEM, "x"), ("/root/other", "y")];
    let notices = merge_notices(&workspace(), "Acme shop", &left);
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
    let details: Vec<String> = merge_notices(&workspace(), "Acme shop", &left)
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
    let [problem] = merge_notices(&workspace(), "Acme shop", &left)
        .try_into()
        .unwrap();
    assert!(problem.detail.contains("bypasses the proxy"));
}

fn settled(left: &[(&str, &str)], problems: &Problems) {
    settle_merge_notices(problems, &workspace(), "Acme shop", left);
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
    assert_eq!(
        merge_notices(&workspace(), "Acme shop", &left),
        Vec::<Problem>::new()
    );
}
