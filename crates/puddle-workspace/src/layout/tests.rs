// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn layout() -> Layout {
    Layout::new(&WorkspaceId::new("acme").unwrap()).unwrap()
}

#[test]
fn paths_sit_under_the_mount_point() {
    let l = layout();
    assert_eq!(l.mount().as_str(), "/workspaces/acme");
    assert_eq!(l.puddle_dir().unwrap().as_str(), "/workspaces/acme/.puddle");
    assert_eq!(
        l.checkout(&CheckoutDir::new("api").unwrap())
            .unwrap()
            .as_str(),
        "/workspaces/acme/api"
    );
}

#[test]
fn clone_request_passes_the_url_as_one_argument_after_double_dash() {
    let dir = CheckoutDir::new("api").unwrap();
    let r = layout()
        .clone_request("-u evil https://h/acme/api.git", &dir)
        .unwrap();
    assert_eq!(r.program, "git");
    assert_eq!(
        r.args,
        [
            "clone",
            "--",
            "-u evil https://h/acme/api.git",
            "/workspaces/acme/api"
        ]
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
                "/workspaces/acme",
                user.name()
            ]
        );
        assert_eq!(r.user.as_deref(), Some("root"));
        assert_eq!(r.timeout, PREPARE_TIMEOUT);
    }
}
