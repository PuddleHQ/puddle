// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests: run the built `puddle` binary as a user would.
#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::process::Command;

/// Runs the binary with no `PUDDLE_` variable of the test run's environment.
fn puddle(args: &[&str]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_puddle"));
    command.args(args);
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("PUDDLE_")) {
        command.env_remove(name);
    }
    command.output().expect("the Puddle binary runs")
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

#[cfg(feature = "serve")] // `puddle serve` is dormant in the shipped build
#[test]
fn serve_without_a_bundled_runtime_exits_1_and_says_why() {
    // The test binary has no `runtime` folder beside it.
    let out = puddle(&["serve", "--port", "0"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.starts_with("puddle: "), "{stderr}");
    assert!(stderr.contains("bundled runtime is missing"), "{stderr}");
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
    assert_eq!(
        restored.rules_every_user(&puddle_store::testing::all_users()),
        live.rules_every_user(&puddle_store::testing::all_users())
    );
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
    assert_eq!(
        copy.rules_every_user(&puddle_store::testing::all_users()),
        live.rules_every_user(&puddle_store::testing::all_users())
    );
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
fn recovery_commands_ignore_a_bad_value_of_an_option_they_never_read() {
    let data = tempfile::tempdir().unwrap();
    for args in [
        &["audit", "status"][..],
        &["secrets", "status"][..],
        &["user", "list"][..],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_puddle"))
            .args(args)
            .arg("--data-dir")
            .arg(data.path())
            .env("PUDDLE_PORT", "not-a-port")
            .env("PUDDLE_REMOTE", "true")
            .output()
            .expect("the Puddle binary runs");
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(
            !stderr.contains("PUDDLE_PORT") && !stderr.contains("remote serving"),
            "{args:?}: {stderr}"
        );
    }
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

#[test]
fn audit_status_and_prune_work_on_a_data_folder_and_say_when_there_is_none() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let missing = puddle(&["audit", "status", "--data-dir", data.to_str().unwrap()]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("no database at")
    );
    drop(
        puddle_store::Store::open(
            &data.join("puddle.db"),
            std::sync::Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    let status = puddle(&["audit", "status", "--data-dir", data.to_str().unwrap()]);
    assert_eq!(status.status.code(), Some(0));
    assert!(
        String::from_utf8(status.stdout)
            .unwrap()
            .contains("kept for 90 days")
    );
    let pruned = puddle(&[
        "audit",
        "prune",
        "--days",
        "7",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(pruned.status.code(), Some(0));
    assert!(
        String::from_utf8(pruned.stdout)
            .unwrap()
            .contains("older than 7 days")
    );
    let refused = puddle(&[
        "audit",
        "prune",
        "--days",
        "0",
        "--data-dir",
        data.to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
}

#[test]
fn audit_without_a_data_dir_looks_in_the_per_user_folder() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(["audit", "status"])
        .env_remove("PUDDLE_DATA_DIR")
        .env("HOME", home.path())
        .env("XDG_DATA_HOME", home.path().join("share"))
        .env("LOCALAPPDATA", home.path().join("local"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("no database at"), "{err}");
    // Windows asks the shell for the per-user folder and ignores the environment, so the
    // expected folder there is what that same lookup gives this process.
    let expected = if cfg!(windows) {
        puddle_fs::data_dir().unwrap()
    } else {
        home.path().to_path_buf()
    };
    assert!(err.contains(expected.to_str().unwrap()), "{err}");
}

#[test]
fn secrets_without_a_valid_command_exits_2() {
    assert_eq!(puddle(&["secrets"]).status.code(), Some(2));
    assert_eq!(puddle(&["secrets", "migrate"]).status.code(), Some(2));
    assert_eq!(
        puddle(&["secrets", "migrate", "--to", "keyring"])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        puddle(&["secrets", "status", "--yes"]).status.code(),
        Some(2)
    );
}

#[test]
fn secrets_status_on_a_folder_that_never_started_says_nothing_is_chosen_and_makes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let out = puddle(&["secrets", "status", "--data-dir", data.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("store: not chosen yet"), "{text}");
    assert!(text.contains("named by rows: 0 secrets"), "{text}");
    assert!(!data.exists(), "status only reads");
}

#[test]
fn secrets_migrate_and_reset_refuse_while_a_host_holds_the_data_folder() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("data");
    let _host = puddle_fs::DataLock::acquire(&data).unwrap();
    let data = data.to_str().unwrap();
    for args in [
        vec!["secrets", "migrate", "--to", "file", "--data-dir", data],
        vec!["secrets", "reset", "--yes", "--data-dir", data],
    ] {
        let out = puddle(&args);
        assert_eq!(out.status.code(), Some(1), "{args:?} {out:?}");
        let text = String::from_utf8(out.stderr).unwrap();
        assert!(text.contains("already using the data folder"), "{text}");
        assert!(text.contains("process ID"), "{text}");
    }
    // Status reads beside the host.
    let out = puddle(&["secrets", "status", "--data-dir", data]);
    assert!(out.status.success(), "{out:?}");
}
