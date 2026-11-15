// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn usage_errors() {
    for (input, needle) in [
        (vec![], "no command"),
        (vec!["frobnicate"], "unknown command frobnicate"),
        (vec!["runtime", "--out"], "--out needs a value"),
        (vec!["runtime", "--bogus"], "unknown argument --bogus"),
        (vec!["notices", "--tag", "x"], "unknown argument --tag"),
        (
            vec!["runtime", "--tag", "v0.7.7", "--from", "/nonexistent"],
            "pass --upstream-tag",
        ),
    ] {
        let err = run(args(&input)).unwrap_err();
        assert!(matches!(err, XtaskError::Usage(_)), "{input:?}: {err}");
        let msg = err.to_string();
        assert!(
            msg.contains(needle) && msg.contains("usage: cargo xtask"),
            "{msg}"
        );
    }
    assert!(run(args(&["help"])).unwrap().starts_with("usage:"));
}

#[test]
fn the_runtime_folder_is_for_this_os_unless_told_otherwise() {
    // No `--os`: Linux first needs the firmware version pinned for the tag (a Parse error
    // for this unpinned one); Windows goes straight on to read the parts (an Io error).
    let err = run(args(&[
        "runtime",
        "--tag",
        "v0.7.7-puddle.1",
        "--from",
        "/nonexistent",
    ]))
    .unwrap_err();
    assert_eq!(
        matches!(err, XtaskError::Parse { .. }),
        HostOs::current() == HostOs::Linux,
        "{err}"
    );
}

#[test]
fn last_value_wins_and_switches_are_seen() {
    let o = Options::parse(
        vec![
            "--out".into(),
            "a".into(),
            "--offline".into(),
            "--out".into(),
            "b".into(),
        ],
        &["--out"],
        &["--offline"],
    )
    .unwrap();
    assert_eq!(o.value("--out"), Some("b"));
    assert!(o.has("--offline"));
    assert!(!o.has("--check"));
    assert_eq!(o.value("--tag"), None);
}

#[test]
fn workspace_root_has_the_workspace_manifest() {
    let text = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap();
    assert!(text.contains("[workspace]"));
    assert!(workspace_root().join("about.toml").is_file());
    assert!(
        workspace_root()
            .join("crates/xtask/about/msb.toml")
            .is_file()
    );
}
