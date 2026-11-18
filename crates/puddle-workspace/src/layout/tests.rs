// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn layout() -> Layout {
    Layout::new().unwrap()
}

#[test]
fn paths_sit_under_the_mount_point() {
    let l = layout();
    assert_eq!(l.mount().as_str(), "/workspace");
    assert_eq!(l.puddle_dir().unwrap().as_str(), "/workspace/.puddle");
    assert_eq!(
        l.checkout(&CheckoutDir::new("api").unwrap())
            .unwrap()
            .as_str(),
        "/workspace/api"
    );
}

#[test]
fn clone_request_runs_the_clone_script_with_the_url_as_one_argument_and_a_stage() {
    let dir = CheckoutDir::new("api").unwrap();
    let r = layout()
        .clone_request("-u evil https://h/acme/api.git", &dir)
        .unwrap();
    assert_eq!(r.program, "sh");
    assert_eq!(
        r.args,
        [
            "-c",
            CLONE_SH,
            "puddle-clone",
            "-u evil https://h/acme/api.git",
            "/workspace/.puddle/cloning/api",
            "/workspace/api"
        ]
    );
    assert!(
        CLONE_SH.contains(r#"git clone -- "$url" "$stage""#),
        "the URL goes to git after --"
    );
    assert_eq!(r.user, None);
}

#[test]
fn sync_request_flushes_everything_as_root() {
    let r = Layout::sync_request();
    assert_eq!(r.program, "sync");
    assert_eq!(r.args, Vec::<String>::new());
    assert_eq!(r.user.as_deref(), Some("root"));
    assert_eq!(r.timeout, SYNC_TIMEOUT);
}

#[test]
fn prepare_request_runs_the_script_as_root_for_the_session_user() {
    for user in [SessionUser::Agent, SessionUser::Root] {
        let r = layout().prepare_request(user);
        assert_eq!(r.program, "sh");
        assert_eq!(
            r.args,
            [
                "-c",
                PREPARE_SH,
                "puddle-prepare",
                "/workspace",
                user.name()
            ]
        );
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.timeout, PREPARE_TIMEOUT);
    }
}

#[test]
fn the_managed_tools_live_in_a_folder_of_the_puddle_directory() {
    assert_eq!(
        layout().mise_dir().unwrap().as_str(),
        "/workspace/.puddle/mise"
    );
}
