// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle doctor` run as a user would, from a folder without the bundled runtime: the report
//! names the missing runtime, the exit code says something must be fixed, and the test boot is
//! never attempted. A real runtime is checked by `vm_doctor` (puddle-vm-tests).
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::path::Path;
use std::process::{Command, Output};

/// Copies the built `puddle` into `dir` (no `runtime/` beside it) and runs it there.
fn puddle_in(dir: &Path, args: &[&str]) -> Output {
    puddle_with_env(dir, args, &[])
}

static COPY_AND_SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// [`puddle_in`] with no `PUDDLE_` variable of the test run's environment, then `env`.
fn puddle_with_env(dir: &Path, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let exe = dir.join(if cfg!(windows) {
        "puddle.exe"
    } else {
        "puddle"
    });
    // Copying an executable while another test thread forks makes that child inherit the open
    // write handle, and the copy's exec then fails with "Text file busy"; one lock covers both.
    let guard = COPY_AND_SPAWN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::fs::copy(env!("CARGO_BIN_EXE_puddle"), &exe).unwrap();
    let mut command = Command::new(&exe);
    command.args(args).current_dir(dir);
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("PUDDLE_")) {
        command.env_remove(name);
    }
    let child = command
        .envs(env.iter().copied())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(guard);
    child.wait_with_output().unwrap()
}

/// The folder the report says it measured the free space of.
fn measured_folder(out: &Output) -> String {
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let checks = v.get("checks").and_then(|c| c.as_array()).unwrap();
    let check = checks
        .iter()
        .find(|c| c.get("id").is_some_and(|id| id == "disk_space"))
        .unwrap();
    check
        .get("summary")
        .and_then(|s| s.as_str())
        .unwrap()
        .to_owned()
}

#[test]
fn the_data_folder_comes_from_the_environment_and_the_config_file_like_serves() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("bin");
    std::fs::create_dir(&dir).unwrap();
    let from_env = root.path().join("env-data");
    let from_file = root.path().join("file-data");
    let from_flag = root.path().join("flag-data");
    for folder in [&from_env, &from_file, &from_flag] {
        std::fs::create_dir(folder).unwrap();
    }
    let config = root.path().join("serve.toml");
    std::fs::write(
        &config,
        format!("data_dir = {:?}\n", from_file.to_str().unwrap()),
    )
    .unwrap();
    let say = |out: Output, folder: &Path| {
        let summary = measured_folder(&out);
        assert!(summary.contains(folder.to_str().unwrap()), "{summary}");
    };
    say(
        puddle_with_env(
            &dir,
            &["doctor", "--json", "--no-boot"],
            &[("PUDDLE_DATA_DIR", &from_env)],
        ),
        &from_env,
    );
    say(
        puddle_with_env(
            &dir,
            &["doctor", "--json", "--no-boot"],
            &[("PUDDLE_CONFIG", &config)],
        ),
        &from_file,
    );
    say(
        puddle_with_env(
            &dir,
            &[
                "doctor",
                "--json",
                "--no-boot",
                "--data-dir",
                from_flag.to_str().unwrap(),
            ],
            &[("PUDDLE_DATA_DIR", &from_env)],
        ),
        &from_flag,
    );
}

#[test]
fn an_unreadable_config_file_stops_the_doctor_with_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle_in(
        dir.path(),
        &["doctor", "--no-boot", "--config", "absent.toml"],
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8(out.stderr.clone()).unwrap();
    assert!(stderr.contains("absent.toml"), "{stderr}");
}

#[test]
fn json_names_the_missing_runtime_and_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle_in(dir.path(), &["doctor", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["puddle_version"], puddle_types::VERSION);
    assert_eq!(v["ok"], false);
    let checks = v["checks"].as_array().unwrap();
    let by_id = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap();
    assert_eq!(by_id("runtime")["finding"], "runtime_missing");
    let fix = by_id("runtime")["fix"].as_str().unwrap();
    assert!(
        fix.contains(&*dir.path().join("runtime").to_string_lossy()),
        "{fix}"
    );
    assert_eq!(by_id("launch")["status"], "skipped");
    assert_eq!(by_id("test_boot")["status"], "skipped");
}

#[test]
fn text_report_has_the_fix_line() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle_in(dir.path(), &["doctor", "--no-boot"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("puddle doctor (puddle "), "{text}");
    assert!(text.contains("FAIL  Bundled runtime"), "{text}");
    assert!(text.contains("fix:  Reinstall Puddle"), "{text}");
    assert!(
        text.contains("to fix before Puddle can run sandboxes"),
        "{text}"
    );
}

#[test]
fn unknown_doctor_flag_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle_in(dir.path(), &["doctor", "--fix"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("unknown argument: --fix")
    );
}

#[test]
fn a_bad_port_variable_does_not_stop_the_doctor() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("bin");
    std::fs::create_dir(&dir).unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let out = puddle_with_env(
        &dir,
        &["doctor", "--json", "--no-boot"],
        &[
            ("PUDDLE_DATA_DIR", &data),
            ("PUDDLE_PORT", Path::new("not-a-port")),
        ],
    );
    let summary = measured_folder(&out);
    assert!(summary.contains(data.to_str().unwrap()), "{summary}");
}
