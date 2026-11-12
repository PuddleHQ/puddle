// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle serve --config`: the built binary refuses a bad file and prints what it would use, and
//! the options of a good file reach a running host.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

use std::future::Future;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;

use puddle::cmd::serve::{self, ServeArgs, options::Value};
use puddle_certs::CorporateRoots;
use puddle_compute::fake::FakeRuntime;
use puddle_host::{Host, HostError, HostOptions, Platform, RuntimeFactory, RuntimeInputs, prepare};
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::MemoryStore;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// Runs the binary with no `PUDDLE_` variable of the test run's environment.
fn puddle(args: &[&str], env: &[(&str, &str)], cwd: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_puddle"));
    command.args(args).current_dir(cwd);
    for (name, _) in std::env::vars().filter(|(name, _)| name.starts_with("PUDDLE_")) {
        command.env_remove(name);
    }
    command.envs(env.iter().copied());
    command.output().expect("the Puddle binary runs")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn a_bad_config_file_refuses_to_start_and_says_where_and_what_to_write() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    let cases = [
        (
            "port = 1\nprot = 2\n",
            ":2: unknown key `prot`",
            "fix the spelling",
        ),
        (
            "port = \"1\"\n",
            ":1: `port` must be a port number",
            "write it as `port = 8080`",
        ),
        ("port = 1\nport = 2\n", ":2:1: ", "a key may appear once"),
        ("port = \n", ":1:", "fix the TOML"),
    ];
    for (body, at, fix) in cases {
        std::fs::write(&file, body).unwrap();
        let out = puddle(
            &["serve", "--config", file.to_str().unwrap()],
            &[],
            dir.path(),
        );
        assert_eq!(out.status.code(), Some(1), "{body}");
        assert_eq!(text(&out.stdout), "");
        let stderr = text(&out.stderr);
        assert!(stderr.starts_with("puddle: "), "{stderr}");
        assert!(stderr.contains(file.to_str().unwrap()), "{stderr}");
        assert!(stderr.contains(at), "{stderr}");
        assert!(stderr.contains(fix), "{stderr}");
    }
}

#[test]
fn a_missing_config_file_refuses_to_start_instead_of_using_the_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle(&["serve", "--config", "absent.toml"], &[], dir.path());
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("cannot read the config file"), "{stderr}");
    assert!(stderr.contains("absent.toml"), "{stderr}");
    // The same through the environment variable.
    let out = puddle(&["serve"], &[("PUDDLE_CONFIG", "absent.toml")], dir.path());
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("cannot read the config file"));
}

#[test]
fn a_bad_environment_value_refuses_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let out = puddle(&["serve"], &[("PUDDLE_PORT", "http")], dir.path());
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.starts_with("puddle: PUDDLE_PORT has the invalid value \"http\""),
        "{stderr}"
    );
}

#[test]
fn print_config_prints_the_effective_options_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(
        &file,
        "port = 9000\nconnection_file = \"api.json\"\nlog = \"warn\"\n",
    )
    .unwrap();
    let out = puddle(
        &[
            "serve",
            "--print-config",
            "--config",
            file.to_str().unwrap(),
            "--port",
            "4000",
        ],
        &[("PUDDLE_LOG", "debug")],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
    let file = file.display();
    assert_eq!(
        text(&out.stdout),
        format!(
            "config = {file} (flag --config)\n\
             port = 4000 (flag --port)\n\
             connection_file = {api} (config file {file})\n\
             data_dir = the per-user data folder (default)\n\
             log = debug (environment PUDDLE_LOG)\n",
            api = dir.path().join("api.json").display(),
        )
    );
}

#[test]
fn print_config_of_a_bad_file_exits_non_zero() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, "nope = 1\n").unwrap();
    let out = puddle(
        &[
            "serve",
            "--print-config",
            "--config",
            file.to_str().unwrap(),
        ],
        &[],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert!(text(&out.stderr).contains("unknown key `nope`"));
}

#[test]
fn a_good_file_gets_past_the_options_to_the_runtime_check() {
    // The test binary has no bundled runtime, so a start that accepted the file stops there.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(&file, "port = 0\ndata_dir = \"data\"\n").unwrap();
    let out = puddle(
        &["serve", "--config", file.to_str().unwrap()],
        &[],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(!stderr.contains("config file"), "{stderr}");
    assert!(stderr.starts_with("puddle: "), "{stderr}");
    // The file's data folder, relative to the file, is the one the host locked.
    assert!(dir.path().join("data").is_dir());
}

/// The machine without its certificate stores and with a runtime that is always there.
struct QuietPlatform;

impl Platform for QuietPlatform {
    fn check_runtime(&self, _: &RuntimeLayout, _: &RuntimeVersion) -> Result<(), HostError> {
        Ok(())
    }

    fn corporate_roots(&self) -> Result<CorporateRoots, HostError> {
        Ok(CorporateRoots::default())
    }
}

struct FakeFactory;

impl RuntimeFactory for FakeFactory {
    type Runtime = FakeRuntime;

    fn open(
        &self,
        _: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<FakeRuntime, HostError>> + Send {
        std::future::ready(Ok(FakeRuntime::new()))
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_started_from_a_config_file_listens_on_the_files_port_and_writes_the_files_connection_file()
 {
    let dir = tempfile::tempdir().unwrap();
    let port = puddle_test_kit::quiet_port();
    std::fs::write(
        dir.path().join("serve.toml"),
        format!("port = {port}\nconnection_file = \"conn/api.json\"\ndata_dir = \"data\"\n"),
    )
    .unwrap();
    std::fs::create_dir(dir.path().join("conn")).unwrap();
    let args = ServeArgs {
        flags: [("config", Value::Path(dir.path().join("serve.toml")))]
            .into_iter()
            .collect(),
        print_config: false,
    };
    let options = serve::options::resolve(&args.flags, &|_| None, dir.path()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut config = serve::config(&options, &exe).unwrap();
    // The one thing a test build has no copy of: the guest agent beside the runtime.
    let agent = dir.path().join("puddle-agent");
    std::fs::write(&agent, b"#!agent\n").unwrap();
    config.guest.agent_binary = agent;
    let prepared = prepare(config, &QuietPlatform).unwrap();
    let mut host_options = HostOptions::default();
    host_options.secret_store = Some(Arc::new(MemoryStore::new()));
    let host = Host::start(prepared, &FakeFactory, host_options)
        .await
        .unwrap();

    assert_eq!(host.url().port(), Some(port));
    let connection = std::fs::read_to_string(dir.path().join("conn").join("api.json")).unwrap();
    assert!(connection.contains(&port.to_string()), "{connection}");
    assert!(dir.path().join("data").join("puddle.db").is_file());

    // The API answers on that port.
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(
            b"GET /api/openapi.json HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    assert!(text(&reply).starts_with("HTTP/1.1 "), "{}", text(&reply));
    host.shutdown().await;
}
