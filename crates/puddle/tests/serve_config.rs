// SPDX-License-Identifier: AGPL-3.0-or-later
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
             log = debug (environment PUDDLE_LOG)\n\
             remote = off (default)\n\
             public_hosts = none (default)\n\
             workspace_base_domain = subdomains of the first public host (default)\n\
             listen_interface = no network listener (default)\n\
             listen_port = 8443 (default)\n\
             allowed_proxies = none (default)\n\
             multi_user = off (default)\n",
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

/// Starts a host from a `serve.toml` in `dir` on a free port, over the fake runtime.
async fn start_fake_host(dir: &Path) -> (Host<FakeRuntime>, u16) {
    let port = puddle_test_kit::quiet_port();
    std::fs::write(
        dir.join("serve.toml"),
        format!("port = {port}\nconnection_file = \"conn/api.json\"\ndata_dir = \"data\"\n"),
    )
    .unwrap();
    std::fs::create_dir(dir.join("conn")).unwrap();
    let args = ServeArgs {
        flags: [("config", Value::Path(dir.join("serve.toml")))]
            .into_iter()
            .collect(),
        print_config: false,
    };
    let options = serve::options::resolve(&args.flags, &|_| None, dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut config = serve::config(&options, &exe).unwrap();
    // The one thing a test build has no copy of: the guest agent beside the runtime.
    let agent = dir.join("puddle-agent");
    std::fs::write(&agent, b"#!agent\n").unwrap();
    config.guest.agent_binary = agent;
    let prepared = prepare(config, &QuietPlatform).unwrap();
    let mut host_options = HostOptions::default();
    host_options.secret_store = Some(Arc::new(MemoryStore::new()));
    let host = Host::start(prepared, &FakeFactory, host_options)
        .await
        .unwrap();

    (host, port)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_started_from_a_config_file_listens_on_the_files_port_and_writes_the_files_connection_file()
 {
    let dir = tempfile::tempdir().unwrap();
    let (host, port) = start_fake_host(dir.path()).await;

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

/// Windows gives the main thread 1 MiB unless the link says otherwise (`build.rs` links 8 MiB), and
/// an overflow is an abort with no message. `puddle serve` starts the host on the main thread
/// (`block_on`), so the start-up must fit in half of the linked stack. Measured on Windows (debug
/// build): it needs between 1024 and 1088 KiB.
#[test]
fn the_host_start_fits_in_half_the_programs_main_thread_stack() {
    let half = env!("PUDDLE_MAIN_STACK_BYTES").parse::<usize>().unwrap() / 2;
    std::thread::Builder::new()
        .stack_size(half)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let dir = tempfile::tempdir().unwrap();
                let (host, port) = start_fake_host(dir.path()).await;
                assert_eq!(host.url().port(), Some(port));
                host.shutdown().await;
            });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn print_config_shows_the_remote_options_and_where_each_came_from() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("serve.toml");
    std::fs::write(
        &file,
        "remote = true\npublic_hosts = [\"puddle.example.org\"]\nlisten_interface = \"192.168.1.10\"\nallowed_proxies = [\"192.168.1.20\"]\n",
    )
    .unwrap();
    let out = puddle(
        &[
            "serve",
            "--print-config",
            "--config",
            file.to_str().unwrap(),
            "--no-remote",
            "--workspace-base-domain",
            "Ws.Example.Net",
        ],
        &[("PUDDLE_LISTEN_PORT", "9443")],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    let file = file.display();
    for line in [
        "remote = false (flag --no-remote)".to_owned(),
        format!("public_hosts = puddle.example.org (config file {file})"),
        "workspace_base_domain = ws.example.net (flag --workspace-base-domain)".to_owned(),
        format!("listen_interface = 192.168.1.10 (config file {file})"),
        "listen_port = 9443 (environment PUDDLE_LISTEN_PORT)".to_owned(),
        format!("allowed_proxies = 192.168.1.20 (config file {file})"),
    ] {
        assert!(shown.lines().any(|l| l == line), "{line}\n{shown}");
    }
}

#[test]
fn remote_options_that_do_not_fit_together_refuse_to_start_and_say_why() {
    let dir = tempfile::tempdir().unwrap();
    for (args, why) in [
        (vec!["serve", "--remote"], "at least one public host"),
        (
            vec!["serve", "--remote", "--public-hosts", "203.0.113.7"],
            "set `workspace_base_domain`",
        ),
        (
            vec![
                "serve",
                "--remote",
                "--public-hosts",
                "puddle.example.org",
                "--listen-interface",
                "192.168.1.10",
            ],
            "`allowed_proxies`",
        ),
        (
            vec![
                "serve",
                "--remote",
                "--public-hosts",
                "puddle.example.org",
                "--listen-port",
                "9443",
            ],
            "set `listen_interface` too",
        ),
    ] {
        let out = puddle(&args, &[], dir.path());
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(text(&out.stdout), "");
        let stderr = text(&out.stderr);
        assert!(stderr.starts_with("puddle: remote serving: "), "{stderr}");
        assert!(stderr.contains(why), "{args:?}: {stderr}");
    }
    // The switch off with the same options is accepted (and prints nothing to refuse).
    let out = puddle(
        &[
            "serve",
            "--print-config",
            "--no-remote",
            "--listen-port",
            "9443",
        ],
        &[],
        dir.path(),
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
}

/// What a request to the host's API at `addr` over `stream` answers with, status line only.
async fn status_of<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    mut stream: S,
    host: &str,
) -> String {
    stream
        .write_all(
            format!("GET /api/health HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply).await;
    text(&reply).lines().next().unwrap_or_default().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_started_with_remote_serving_answers_a_proxy_over_tls_and_a_loopback_client_as_before()
 {
    let dir = tempfile::tempdir().unwrap();
    let port = puddle_test_kit::quiet_port();
    let listen = puddle_test_kit::quiet_port();
    std::fs::write(
        dir.path().join("serve.toml"),
        format!(
            "port = {port}\ndata_dir = \"data\"\nremote = true\npublic_hosts = [\"puddle.example.org\", \"203.0.113.7\"]\n\
             listen_interface = \"127.0.0.1\"\nlisten_port = {listen}\nallowed_proxies = [\"127.0.0.1\"]\n"
        ),
    )
    .unwrap();
    let flags = [("config", Value::Path(dir.path().join("serve.toml")))]
        .into_iter()
        .collect();
    let options = serve::options::resolve(&flags, &|_| None, dir.path()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut config = serve::config(&options, &exe).unwrap();
    let agent = dir.path().join("puddle-agent");
    std::fs::write(&agent, b"#!agent\n").unwrap();
    config.guest.agent_binary = agent;
    let prepared = prepare(config, &QuietPlatform).unwrap();
    let mut host_options = HostOptions::default();
    host_options.secret_store = Some(Arc::new(MemoryStore::new()));
    let host = Host::start(prepared, &FakeFactory, host_options)
        .await
        .unwrap();

    // The loopback listener: the install token's address works, a public name is not the token's.
    let tcp = || tokio::net::TcpStream::connect(("127.0.0.1", port));
    assert_eq!(
        status_of(tcp().await.unwrap(), &format!("127.0.0.1:{port}")).await,
        "HTTP/1.1 401 Unauthorized"
    );
    assert_eq!(
        status_of(tcp().await.unwrap(), "puddle.example.org").await,
        "HTTP/1.1 401 Unauthorized"
    );
    assert_eq!(
        status_of(tcp().await.unwrap(), "other.example.org").await,
        "HTTP/1.1 421 Misdirected Request"
    );

    // The network listener: TLS with the certificate the host printed, the public name only.
    let info = host.remote_listener().expect("a network listener").clone();
    assert_eq!(info.addr.port(), listen);
    assert_eq!(info.fingerprint.len(), 64);
    let pem = std::fs::read(&info.certificate_file).unwrap();
    assert!(!text(&pem).contains("PRIVATE KEY"));
    let der = rustls_pemfile_free(&pem);
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(rustls::pki_types::CertificateDer::from(der))
        .unwrap();
    let client = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(client));
    let over_tls = |host_header: &'static str| {
        let connector = connector.clone();
        async move {
            let tcp = tokio::net::TcpStream::connect(info.addr).await.unwrap();
            let name = rustls::pki_types::ServerName::try_from("puddle.example.org").unwrap();
            status_of(connector.connect(name, tcp).await.unwrap(), host_header).await
        }
    };
    assert_eq!(
        over_tls("puddle.example.org").await,
        "HTTP/1.1 401 Unauthorized"
    );
    assert_eq!(
        over_tls("127.0.0.1").await,
        "HTTP/1.1 421 Misdirected Request"
    );

    // The host gave the API its sign-in secrets: under the public name the sign-in page can ask
    // whether the owner account is still to be made, with no token.
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream
        .write_all(
            b"GET /api/auth/status HTTP/1.1\r\nHost: puddle.example.org\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = Vec::new();
    let _ = stream.read_to_end(&mut reply).await;
    let reply = text(&reply);
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(
        reply.ends_with(r#"{"setup_open":true,"password_login":true}"#),
        "{reply}"
    );
    host.shutdown().await;
}

/// The certificate of a one-block PEM file, as DER.
fn rustls_pemfile_free(pem: &[u8]) -> Vec<u8> {
    pem::parse(pem).unwrap().into_contents()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listener_certificate_that_cannot_be_kept_stops_the_start_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // A file where the certificate's folder belongs.
    std::fs::write(data.join("remote-cert"), b"in the way").unwrap();
    let flags = [
        ("data_dir", Value::Path(data)),
        ("remote", Value::Bool(true)),
        (
            "public_hosts",
            Value::Hosts(vec!["puddle.example.org".to_owned()]),
        ),
        ("listen_interface", Value::Ip("127.0.0.1".parse().unwrap())),
        ("listen_port", Value::Port(0)),
        (
            "allowed_proxies",
            Value::IpList(vec!["127.0.0.1".parse().unwrap()]),
        ),
    ]
    .into_iter()
    .collect();
    let options = serve::options::resolve(&flags, &|_| None, dir.path()).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut config = serve::config(&options, &exe).unwrap();
    let agent = dir.path().join("puddle-agent");
    std::fs::write(&agent, b"#!agent\n").unwrap();
    config.guest.agent_binary = agent;
    let prepared = prepare(config, &QuietPlatform).unwrap();
    let mut host_options = HostOptions::default();
    host_options.secret_store = Some(Arc::new(MemoryStore::new()));
    let Err(err) = Host::start(prepared, &FakeFactory, host_options).await else {
        panic!("the start should fail");
    };
    assert!(
        err.to_string()
            .contains("the network listener's certificate"),
        "{err}"
    );
}
