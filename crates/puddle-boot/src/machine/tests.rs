// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_types::{ApplyKind, Merged};

use super::*;

#[test]
fn settings_forward_by_process_and_guard_git_auth() {
    let f = machine_settings(3128, SessionUser::Root).unwrap();
    assert_eq!(f.path().as_str(), MACHINE_SETTINGS_GUEST);
    assert_eq!(
        machine_settings(3128, SessionUser::Agent)
            .unwrap()
            .path()
            .as_str(),
        "/home/agent/.vscode-server/data/Machine/settings.json"
    );
    assert_eq!(f.mode(), 0o644);
    let ApplyKind::Merge(spec) = f.apply() else {
        panic!("the Machine settings are merged, not replaced")
    };
    assert_eq!(spec.format(), MergeFormat::Jsonc);
    let v: serde_json::Value = serde_json::from_slice(f.contents()).unwrap();
    assert_eq!(v["remote.autoForwardPortsSource"], "process");
    assert_eq!(v["remote.autoForwardPortsFallback"], 0);
    assert_eq!(
        v["remote.portsAttributes"]["3128"]["onAutoForward"],
        "ignore"
    );
    assert_eq!(v["remote.otherPortsAttributes"]["onAutoForward"], "notify");
    assert_eq!(v["github.gitAuthentication"], false);
    assert_eq!(v["git.terminalAuthentication"], false);
    assert!(f.contents().ends_with(b"}\n"));
}

#[test]
fn puddle_owns_leaves_not_whole_port_tables() {
    let keys = machine_settings(3128, SessionUser::Root).unwrap();
    let ApplyKind::Merge(spec) = keys.apply() else {
        panic!()
    };
    let keys = spec.keys();
    // A user's own `remote.portsAttributes` entries and other ports' attributes are theirs.
    assert!(keys.contains(&vec![
        "remote.portsAttributes".to_owned(),
        "3128".to_owned()
    ]));
    assert!(!keys.contains(&vec!["remote.portsAttributes".to_owned()]));
    assert!(!keys.contains(&vec!["remote.otherPortsAttributes".to_owned()]));
    assert_eq!(keys.len(), 6);
}

#[test]
fn any_agent_port_gives_a_valid_spec() {
    for port in [0, 1, 3128, u16::MAX] {
        let _ = machine_settings(port, SessionUser::Agent).unwrap();
    }
}

#[test]
fn the_users_own_machine_settings_survive_a_boot_and_the_guard_is_kept() {
    let user = "// mine\n{\n  \"editor.fontSize\": 14, // big\n  \"github.gitAuthentication\": true,\n  \"remote.portsAttributes\": { \"8080\": { \"label\": \"web\" }, },\n}\n";
    let f = machine_settings(3128, SessionUser::Root).unwrap();
    let ApplyKind::Merge(spec) = f.apply() else {
        panic!()
    };
    let Merged::Write(out) = spec.apply(Some(user.as_bytes()), &[]).unwrap() else {
        panic!("the file changes")
    };
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.starts_with("// mine\n{\n  \"editor.fontSize\": 14, // big\n"),
        "{out}"
    );
    assert!(out.contains("\"8080\": { \"label\": \"web\" }"), "{out}");
    assert!(out.contains("\"3128\""), "{out}");
    // The guard wins over the user's own value, at every boot.
    assert!(out.contains("\"github.gitAuthentication\": false"), "{out}");
    assert_eq!(
        spec.apply(Some(out.as_bytes()), &spec.keys()).unwrap(),
        Merged::Unchanged
    );
}

/// Desktop attach runs VS Code's own server and reads this file: Puddle's trust of the browser
/// editor's folder must not reach it, so the desktop editor keeps its Workspace Trust prompt.
#[test]
fn settings_leave_workspace_trust_to_the_desktop_editor() {
    let text = String::from_utf8(
        machine_settings(3128, SessionUser::Root)
            .unwrap()
            .contents()
            .to_vec(),
    )
    .unwrap();
    assert!(!text.contains("workspace.trust"), "{text}");
    assert!(!text.contains("workspaceTrust"), "{text}");
}

#[test]
fn settings_name_no_credential_helper() {
    let text = String::from_utf8(
        machine_settings(3128, SessionUser::Root)
            .unwrap()
            .contents()
            .to_vec(),
    )
    .unwrap();
    assert!(!text.to_lowercase().contains("credential"));
}

#[test]
fn the_machine_settings_path_follows_the_session_user() {
    assert_eq!(
        machine_settings_path(SessionUser::Root),
        MACHINE_SETTINGS_GUEST
    );
    assert_eq!(
        machine_settings_path(SessionUser::Agent),
        "/home/agent/.vscode-server/data/Machine/settings.json"
    );
}
