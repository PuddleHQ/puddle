// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

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
fn the_log_filter_is_info_unless_puddle_log_says_otherwise() {
    let level = |value: Option<&str>, target: &str, level: tracing::Level| {
        log_filter(value.map(OsStr::new))
            .unwrap()
            .would_enable(target, &level)
    };
    assert!(level(None, "puddle_host", tracing::Level::INFO));
    assert!(!level(None, "puddle_host", tracing::Level::DEBUG));
    assert!(level(Some("trace"), "puddle_host", tracing::Level::TRACE));
    assert!(!level(Some("trace"), "hyper", tracing::Level::DEBUG));
    assert!(level(
        Some("trace,hyper=debug"),
        "hyper",
        tracing::Level::DEBUG
    ));
    let err = log_filter(Some(OsStr::new("loud"))).unwrap_err();
    assert!(err.starts_with("PUDDLE_LOG"), "{err}");
    assert!(err.contains("\"loud\""), "{err}");
}

#[test]
fn no_options_is_the_default() {
    assert_eq!(serve(&[]), Ok(ServeArgs::default()));
}

#[test]
fn port_and_connection_file() {
    let a = serve(&["--port", "4000", "--connection-file", "/tmp/c.json"]).unwrap();
    assert_eq!(a.port, Some(4000));
    // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
    assert_eq!(a.connection_file, Some(PathBuf::from("/tmp/c.json")));
}

#[test]
fn bad_input_is_a_usage_error() {
    assert!(serve(&["--port"]).is_err());
    assert!(serve(&["--port", "x"]).is_err());
    assert!(serve(&["--port", "70000"]).is_err());
    assert!(serve(&["--port", "1", "--port", "2"]).is_err());
    assert!(serve(&["--connection-file"]).is_err());
    assert!(serve(&["--wat"]).is_err());
}

#[test]
fn the_arguments_reach_the_configuration() {
    let args = ServeArgs {
        port: Some(4123),
        connection_file: Some(PathBuf::from("/x/api.json")),
    };
    let exe = std::env::current_exe().unwrap();
    if let Ok(config) = config(&args, &exe) {
        assert_eq!(config.api.port, 4123);
        assert_eq!(
            config.api.connection_file,
            Some(PathBuf::from("/x/api.json"))
        );
    }
}
