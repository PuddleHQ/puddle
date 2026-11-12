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
        "--remote",
        "--public-hosts",
        "puddle.example.org,ws.example.org:8443",
        "--workspace-base-domain",
        "Ws.Example.Net",
        "--listen-interface",
        "192.168.1.10",
        "--listen-port",
        "9443",
        "--allowed-proxies",
        "192.168.1.20, 10.0.0.1",
        "--multi-user",
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
    assert_eq!(a.flags.get("remote"), Some(&Value::Bool(true)));
    assert_eq!(
        a.flags.get("public_hosts"),
        Some(&Value::Hosts(vec![
            "puddle.example.org".into(),
            "ws.example.org:8443".into()
        ]))
    );
    assert_eq!(
        a.flags.get("workspace_base_domain"),
        Some(&Value::Domain("ws.example.net".into()))
    );
    assert_eq!(
        a.flags.get("listen_interface"),
        Some(&Value::Ip("192.168.1.10".parse().unwrap()))
    );
    assert_eq!(a.flags.get("listen_port"), Some(&Value::Port(9443)));
    assert_eq!(
        a.flags.get("allowed_proxies"),
        Some(&Value::IpList(vec![
            "192.168.1.20".parse().unwrap(),
            "10.0.0.1".parse().unwrap()
        ]))
    );
    assert_eq!(a.flags.get("multi_user"), Some(&Value::Bool(true)));
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

#[test]
fn a_switch_takes_no_value_and_has_a_negated_flag() {
    assert_eq!(
        serve(&["--remote"]).unwrap().flags.get("remote"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        serve(&["--no-remote"]).unwrap().flags.get("remote"),
        Some(&Value::Bool(false))
    );
    // The next argument is a flag of its own, not the switch's value.
    let both = serve(&["--remote", "--port", "1"]).unwrap();
    assert_eq!(both.flags.get("port"), Some(&Value::Port(1)));
    assert!(serve(&["--remote", "--no-remote"]).is_err());
    assert!(serve(&["--no-remote", "--remote"]).is_err());
    assert!(serve(&["--remote", "--remote"]).is_err());
    assert!(serve(&["--remote", "true"]).is_err());
    assert!(serve(&["--no-port"]).is_err());
}

#[test]
fn a_bad_value_for_a_remote_flag_is_a_usage_error() {
    assert_eq!(
        serve(&["--public-hosts", "https://x.example"])
            .unwrap_err()
            .to_string(),
        "--public-hosts must be a comma-separated list of host names, each with an optional port, such as puddle.example.org or puddle.example.org:8443, found \"https://x.example\""
    );
    assert!(serve(&["--listen-interface", "example.org"]).is_err());
    assert!(serve(&["--allowed-proxies", "1.2.3"]).is_err());
    assert!(serve(&["--workspace-base-domain", "a b"]).is_err());
    assert!(serve(&["--listen-port", "x"]).is_err());
    assert_eq!(
        serve(&["--public-hosts"]),
        Err(UsageError::MissingArgument("--public-hosts <host,...>"))
    );
}

fn config_for(env: &[(&'static str, &'static str)]) -> Result<HostConfig, String> {
    let dir = tempfile::tempdir().unwrap();
    let vars: std::collections::BTreeMap<&str, &str> = env.iter().copied().collect();
    let mut flags = Flags::new();
    flags.insert("data_dir", Value::Path(dir.path().join("data")));
    let options = resolve(
        &flags,
        &|name| vars.get(name).map(std::ffi::OsString::from),
        dir.path(),
    )
    .map_err(|e| e.to_string())?;
    let exe = std::env::current_exe().unwrap();
    config(&options, &exe).map_err(|e| e.to_string())
}

#[test]
fn remote_serving_off_leaves_the_api_settings_as_they_were() {
    let config = config_for(&[]).unwrap();
    assert!(config.api.remote.is_none());
    // Off with the rest set: still nothing.
    let config = config_for(&[("PUDDLE_PUBLIC_HOSTS", "puddle.example.org")]).unwrap();
    assert!(config.api.remote.is_none());
}

#[test]
fn the_remote_options_reach_the_host_configuration() {
    let config = config_for(&[
        ("PUDDLE_REMOTE", "true"),
        ("PUDDLE_PUBLIC_HOSTS", "puddle.example.org"),
        ("PUDDLE_LISTEN_INTERFACE", "192.168.1.10"),
        ("PUDDLE_ALLOWED_PROXIES", "192.168.1.20"),
    ])
    .unwrap();
    let remote = config.api.remote.unwrap();
    assert_eq!(
        remote.serving.public_hosts()[0].authority(),
        "puddle.example.org"
    );
    let listener = remote.network.unwrap();
    assert_eq!(listener.port(), options::DEFAULT_LISTEN_PORT);
    assert_eq!(listener.interface().to_string(), "192.168.1.10");
}

#[test]
fn several_users_are_off_unless_an_option_turns_them_on() {
    assert!(!config_for(&[]).unwrap().api.multi_user);
    assert!(
        config_for(&[("PUDDLE_MULTI_USER", "true")])
            .unwrap()
            .api
            .multi_user
    );
    assert!(
        !config_for(&[("PUDDLE_MULTI_USER", "false")])
            .unwrap()
            .api
            .multi_user
    );
}

#[test]
fn options_that_do_not_fit_together_refuse_to_start_with_the_whole_message() {
    let err = config_for(&[("PUDDLE_REMOTE", "true")]).unwrap_err();
    assert!(err.contains("at least one public host"), "{err}");
    let args = ServeArgs {
        flags: {
            let mut flags = Flags::new();
            flags.insert("remote", Value::Bool(true));
            flags
        },
        print_config: false,
    };
    // `effective` reads the real environment, which the test run may set; the check itself is the
    // same function the options call.
    let options = resolve(&args.flags, &|_| None, std::path::Path::new("/")).unwrap();
    assert!(options.remote().is_err());
}

#[test]
fn the_start_up_lines_name_the_listener_its_fingerprint_and_its_certificate_file() {
    let listener = puddle_host::RemoteListener {
        addr: "192.168.1.10:8443".parse().unwrap(),
        fingerprint: "ab".repeat(32),
        certificate_file: "/srv/remote-cert/listener.crt".into(),
    };
    let lines = listener_lines(&listener);
    assert_eq!(lines[0], "Network listener (TLS) on 192.168.1.10:8443");
    assert_eq!(
        lines[1],
        format!("  certificate SHA-256: {}", "ab".repeat(32))
    );
    assert_eq!(
        lines[2],
        "  certificate file: /srv/remote-cert/listener.crt"
    );
}
