// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use options::{OPTIONS, Value};

#[test]
fn the_exit_names_each_sandbox_that_did_not_stop_or_trim_cleanly_and_why() {
    use puddle_lifecycle::{ShutdownReport, StopOutcome, TrimOutcome, WorkspaceOutcome};
    let outcome = |name: &str, trim, stop| WorkspaceOutcome {
        sandbox: puddle_types::SandboxName::new(name).unwrap(),
        trim,
        stop,
    };
    let clean = ShutdownReport {
        sandboxes: vec![outcome("fine", TrimOutcome::Trimmed, StopOutcome::Stopped)],
    };
    assert_eq!(shutdown_lines(&clean).len(), 0);
    assert_eq!(
        format!("{:?}", exit_after(&clean)),
        format!("{:?}", ExitCode::SUCCESS)
    );
    let report = ShutdownReport {
        sandboxes: vec![
            outcome("fine", TrimOutcome::Trimmed, StopOutcome::Stopped),
            outcome(
                "slow",
                TrimOutcome::Error("no answer".to_owned()),
                StopOutcome::Forced,
            ),
        ],
    };
    let lines = shutdown_lines(&report);
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(lines[0].contains("disk of slow was not trimmed: no answer"));
    assert!(lines[2].contains("slow: it did not shut down in time"));
    assert!(lines[3].contains("check the workspace volumes"));
    assert_eq!(
        format!("{:?}", exit_after(&report)),
        format!("{:?}", ExitCode::FAILURE)
    );
}

fn serve(args: &[&str]) -> Result<ServeArgs, UsageError> {
    match parse(args.iter()) {
        Ok(Command::Serve(a)) => Ok(a),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

#[test]
fn no_options_is_the_default() {
    assert_eq!(serve(&[]), Ok(ServeArgs::default()));
}

#[test]
fn every_option_has_a_flag() {
    let a = serve(&[
        "--port",
        "4000",
        "--connection-file",
        "c.json",
        "--data-dir",
        "d",
        "--log",
        "debug",
        "--config",
        "s.toml",
        "--print-config",
    ])
    .unwrap();
    assert!(a.print_config);
    assert_eq!(a.flags.get("port"), Some(&Value::Port(4000)));
    assert_eq!(
        a.flags.get("connection_file"),
        Some(&Value::Path("c.json".into()))
    );
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
    assert_eq!(a.flags.get("log"), Some(&Value::Log("debug".into())));
    assert_eq!(a.flags.get("config"), Some(&Value::Path("s.toml".into())));
    assert_eq!(a.flags.len(), OPTIONS.len());
}

#[test]
fn bad_input_is_a_usage_error() {
    assert_eq!(
        serve(&["--port"]),
        Err(UsageError::MissingArgument("--port <n>"))
    );
    assert_eq!(
        serve(&["--config"]),
        Err(UsageError::MissingArgument("--config <path>"))
    );
    assert_eq!(
        serve(&["--port", "x"]).unwrap_err().to_string(),
        "--port must be a port number from 0 to 65535, found \"x\""
    );
    assert!(serve(&["--port", "70000"]).is_err());
    assert!(serve(&["--port", "1", "--port", "2"]).is_err());
    assert!(serve(&["--print-config", "--print-config"]).is_err());
    assert!(serve(&["--log", "loud"]).is_err());
    assert!(serve(&["--data-dir", ""]).is_err());
    assert!(serve(&["--connection-file"]).is_err());
    assert!(serve(&["--wat"]).is_err());
}

#[test]
fn the_options_reach_the_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let mut flags = Flags::new();
    flags.insert("port", Value::Port(4123));
    flags.insert("data_dir", Value::Path(dir.path().join("data")));
    flags.insert("connection_file", Value::Path(dir.path().join("api.json")));
    let options = resolve(&flags, &|_| None, dir.path()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let config = config(&options, &exe).unwrap();
    assert_eq!(config.api.port, 4123);
    assert_eq!(
        config.api.connection_file,
        Some(dir.path().join("api.json"))
    );
    assert_eq!(config.paths.data(), dir.path().join("data"));
}
