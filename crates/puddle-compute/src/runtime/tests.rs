// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_default_checkout_rule_names_the_folder_and_what_is_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let err = crate::fake::FakeRuntime::default()
        .check_checkout(dir.path())
        .unwrap_err();
    let reason = err.to_string();
    assert!(matches!(err, ComputeError::InvalidSpec { .. }), "{reason}");
    assert!(
        reason.contains(&format!("the checkout {} ", dir.path().display())),
        "{reason}"
    );
}

#[test]
fn info_names_parse_only_when_valid() {
    let ours = SandboxInfo {
        name: "box".into(),
        status: WorkspaceStatus::Running,
        puddle_owned: true,
        root_disk: None,
        routes: Vec::new(),
    };
    assert_eq!(ours.sandbox_name().unwrap().as_str(), "box");
    let foreign = SandboxInfo {
        name: "Foreign_Box".into(),
        status: WorkspaceStatus::Stopped,
        puddle_owned: false,
        root_disk: None,
        routes: Vec::new(),
    };
    assert!(foreign.sandbox_name().is_none());
    let vol = VolumeInfo {
        name: "ws-a".into(),
        size: DiskSize::mib(1),
        used: None,
        holder: None,
    };
    assert_eq!(vol.volume_name().unwrap().as_str(), "ws-a");
    let vol = VolumeInfo {
        name: "Bad".into(),
        ..vol
    };
    assert!(vol.volume_name().is_none());
}

#[test]
fn image_env_last_value_wins() {
    let c = ImageConfig {
        env: vec![
            ("PATH".into(), "/a".into()),
            ("X".into(), "1".into()),
            ("PATH".into(), "/b".into()),
        ],
        ..ImageConfig::default()
    };
    assert_eq!(c.env_var("PATH"), Some("/b"));
    assert_eq!(c.env_var("NOPE"), None);
}
