// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests: run the built `puddle` binary as a user would.
#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::process::Command;

fn puddle(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(args)
        .output()
        .expect("the Puddle binary runs")
}

#[test]
fn version_prints_name_and_version() {
    let out = puddle(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim_end(),
        format!("puddle {}", puddle_types::VERSION)
    );
}

#[test]
fn help_prints_usage() {
    let out = puddle(&["--help"]);
    assert!(out.status.success());
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("usage: puddle")
    );
}

#[test]
fn unknown_argument_exits_2_with_usage_on_stderr() {
    let out = puddle(&["--bogus"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("unknown argument: --bogus"), "{stderr}");
    assert!(stderr.contains("usage: puddle"), "{stderr}");
}

#[test]
fn serve_without_a_bundled_runtime_exits_1_and_says_why() {
    // The test binary has no `runtime` folder beside it.
    let out = puddle(&["serve", "--port", "0"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with("puddle: "), "{stderr}");
}

#[test]
fn serve_rejects_a_bad_port_with_exit_2() {
    let out = puddle(&["serve", "--port", "nope"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn backup_copies_a_live_database_and_names_what_it_leaves_out() {
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    // The store stays open, as it is while a host runs.
    let live = puddle_store::Store::open(
        &data.join("puddle.db"),
        Arc::new(puddle_store::SystemClock),
        puddle_store::Limits::default(),
    )
    .unwrap();
    std::fs::write(
        data.join("workspaces.json"),
        br#"{"version":2,"workspaces":[]}"#,
    )
    .unwrap();
    let out = root.path().join("out");
    let result = puddle(&[
        "backup",
        "--data-dir",
        data.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{result:?}");
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(
        stdout.contains("not included: stored logins and secrets")
            && stdout.contains("account passwords and open invites"),
        "{stdout}"
    );
    let restored = puddle_store::Store::open(
        &out.join("puddle.db"),
        Arc::new(puddle_store::SystemClock),
        puddle_store::Limits::default(),
    )
    .unwrap();
    assert_eq!(restored.rules_every_user(), live.rules_every_user());
}

#[test]
fn backup_reads_the_folder_the_environment_names_not_the_per_user_one() {
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let live = puddle_store::Store::open(
        &data.join("puddle.db"),
        Arc::new(puddle_store::SystemClock),
        puddle_store::Limits::default(),
    )
    .unwrap();
    let out = root.path().join("out");
    let result = Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(["backup", out.to_str().unwrap()])
        .env("PUDDLE_DATA_DIR", &data)
        .output()
        .expect("the Puddle binary runs");
    assert!(result.status.success(), "{result:?}");
    let copy = puddle_store::Store::open(
        &out.join("puddle.db"),
        Arc::new(puddle_store::SystemClock),
        puddle_store::Limits::default(),
    )
    .unwrap();
    assert_eq!(copy.rules_every_user(), live.rules_every_user());
}

#[test]
fn backup_ignores_a_bad_port_variable_it_never_reads() {
    use std::sync::Arc;
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    drop(
        puddle_store::Store::open(
            &data.join("puddle.db"),
            Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    let out = root.path().join("out");
    let result = Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(["backup", out.to_str().unwrap()])
        .env("PUDDLE_DATA_DIR", &data)
        .env("PUDDLE_PORT", "not-a-port")
        .output()
        .expect("the Puddle binary runs");
    assert!(result.status.success(), "{result:?}");
    assert!(out.join("puddle.db").is_file());
}

#[test]
fn backup_without_a_database_exits_1_and_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let out = root.path().join("out");
    let result = puddle(&[
        "backup",
        "--data-dir",
        root.path().join("empty").to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8(result.stderr)
            .unwrap()
            .contains("no database at")
    );
    assert!(!out.exists());
}

#[test]
fn backup_without_a_destination_exits_2() {
    assert_eq!(puddle(&["backup"]).status.code(), Some(2));
}

#[test]
fn backup_without_a_workspace_list_says_it_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    drop(
        puddle_store::Store::open(
            &data.join("puddle.db"),
            std::sync::Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    let out = root.path().join("out");
    let result = puddle(&[
        "backup",
        "--data-dir",
        data.to_str().unwrap(),
        out.to_str().unwrap(),
    ]);
    assert!(result.status.success(), "{result:?}");
    let stdout = String::from_utf8(result.stdout).unwrap();
    assert!(stdout.contains("workspace list absent"), "{stdout}");
}

#[test]
fn backup_to_a_relative_destination_lands_in_the_working_folder() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    drop(
        puddle_store::Store::open(
            &data.join("puddle.db"),
            std::sync::Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    let result = Command::new(env!("CARGO_BIN_EXE_puddle"))
        .current_dir(root.path())
        .args(["backup", "--data-dir", data.to_str().unwrap(), "out"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
    assert!(root.path().join("out").join("puddle.db").is_file());
}

#[test]
fn user_reset_without_a_name_exits_2() {
    assert_eq!(puddle(&["user", "reset"]).status.code(), Some(2));
    assert_eq!(puddle(&["user"]).status.code(), Some(2));
}

#[test]
fn user_reset_takes_the_same_data_folder_options_as_backup() {
    let twice = puddle(&[
        "user",
        "reset",
        "--data-dir",
        "a",
        "--data-dir",
        "b",
        "owner",
    ]);
    assert_eq!(twice.status.code(), Some(2));
    let missing = puddle(&["user", "reset", "owner", "--config"]);
    assert_eq!(missing.status.code(), Some(2));
}

#[test]
fn user_reset_still_reads_the_whole_option_table() {
    let out = Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(["user", "reset", "owner"])
        .env("PUDDLE_PORT", "not-a-port")
        .output()
        .expect("the Puddle binary runs");
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("PUDDLE_PORT")
    );
}

#[test]
fn user_reset_prints_one_link_and_sets_no_password() {
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    drop(
        puddle_store::Store::open(
            &data.join("puddle.db"),
            Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    let config = root.path().join("serve.toml");
    std::fs::write(
        &config,
        "data_dir = \"data\"\nremote = true\npublic_hosts = [\"puddle.example.org\"]\n",
    )
    .unwrap();
    let out = puddle(&[
        "user",
        "reset",
        "--config",
        config.to_str().unwrap(),
        "owner",
    ]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let link = stdout.trim_end();
    assert!(
        link.starts_with("https://puddle.example.org/invite#"),
        "{stdout}"
    );
    assert_eq!(
        stdout.lines().count(),
        1,
        "the link is the only thing on stdout"
    );
    assert!(stderr.contains("password is removed"), "{stderr}");
    assert!(
        !stderr.contains(link.rsplit('#').next().unwrap()),
        "the secret is only on stdout"
    );

    let unknown = puddle(&[
        "user",
        "reset",
        "--config",
        config.to_str().unwrap(),
        "ghost",
    ]);
    assert_eq!(unknown.status.code(), Some(1));
    assert!(
        String::from_utf8(unknown.stderr)
            .unwrap()
            .contains("no user named ghost")
    );
}
