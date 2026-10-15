// SPDX-License-Identifier: GPL-3.0-or-later
//! The Git tests' rig: a fake Git host (real `git http-backend` behind TLS), a workspace `box`
//! whose injector is the real one over a real in-memory store, the real proxy with TLS
//! termination for the host's name, and the way a client reaches it: the real guest agent where
//! one exists (Unix), a stream bridge into the route where not (Windows, which has no agent).
//! The client is real `git` or a hand-written HTTPS client trusting the workspace's CA.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the pieces")]

use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use puddle_ca::{CaBuilder, WorkspaceCa};
use puddle_e2e::lab::{GitHost, LOCAL, Versions, clean_git};
use puddle_inject::testing::World;
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::AnyAddress;
use puddle_proxy::{BoxFuture, Proxy, Resolver, Route, Termination, TerminationSet, Terminations};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::{DomainName, NullSink};
use puddle_upstream::TlsClient;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The host's name in the guest's world.
pub(crate) const NAME: &str = "bound.test";
/// The tokens of the two identities the rig makes; only the fake host should ever see them.
pub(crate) const WORK: &str = "CANARY-WORK-token-4f81a9";
pub(crate) const PERSONAL: &str = "CANARY-PERSONAL-token-77c0d2";

/// Resolves the host's name to the fake host's address, any port.
#[derive(Debug)]
struct Fixed(SocketAddr);

impl Resolver for Fixed {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        _port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        let found = (name.as_str() == NAME).then_some(self.0);
        Box::pin(async move {
            found
                .map(|a| vec![a])
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such name"))
        })
    }
}

/// The fake host, the workspace and the way in.
pub(crate) struct GitRig {
    pub(crate) host: GitHost,
    pub(crate) world: World,
    pub(crate) ca: Arc<WorkspaceCa>,
    /// The loopback address a client names as its HTTP proxy.
    pub(crate) proxy: SocketAddr,
    /// Where git runs, with its own `HOME`.
    pub(crate) home: tempfile::TempDir,
    _route: Route,
    _root: IpcRoot,
    _way_in: WayIn,
}

#[cfg(unix)]
type WayIn = puddle_agent::Agent;
#[cfg(not(unix))]
type WayIn = tokio::task::JoinHandle<()>;

/// What `git` printed.
pub(crate) struct Ran {
    pub(crate) ok: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl GitRig {
    pub(crate) async fn new(versions: Versions) -> Self {
        Self::with_certificate(versions, NAME).await
    }

    /// As [`Self::new`], with a host whose certificate is valid for `cert_name`.
    pub(crate) async fn with_certificate(versions: Versions, cert_name: &str) -> Self {
        let host = GitHost::start_named(NAME, cert_name, versions).await;
        host.accept_token(WORK);
        host.accept_token(PERSONAL);
        // Private: need a token. Public: anyone reads, a push needs a token.
        host.add_repo("acme/web", true);
        host.add_repo("acme/readonly", true);
        host.add_repo("acme/unlisted", true);
        host.add_repo("acme/open", false);
        host.add_repo("someone/else", true);

        let world = World::new();
        world.identity("Work", NAME, &["acme"], false, WORK);
        world.identity("Personal", NAME, &[], true, PERSONAL);
        world.list("bound.test/acme/web", true, true);
        world.list("bound.test/acme/readonly", true, false);
        world
            .store
            .add_rule(&NewRule {
                scope: Scope::Workspace(world.workspace.clone()),
                pattern: Pattern::parse(NAME).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();

        let ca = Arc::new(CaBuilder::new("Puddle git test CA").build().unwrap());
        let terminations = Arc::new(Terminations::new());
        terminations.insert(
            world.workspace.clone(),
            Termination::new(
                TerminationSet::parse([NAME]).unwrap(),
                Arc::clone(&ca),
                world.injector.clone(),
            ),
        );
        let proxy = Arc::new(
            Proxy::new(world.store.clone(), Arc::new(NullSink))
                .with_connection_log(world.store.clone())
                .with_resolver(Arc::new(Fixed(host.addr())))
                .with_address_check(Arc::new(AnyAddress))
                .with_termination(
                    terminations as Arc<dyn puddle_proxy::TerminationSource>,
                    TlsClient::new([host.root().clone()]).unwrap(),
                ),
        );
        let root = IpcRoot::new().unwrap();
        let route = proxy.serve_route(root.listen().unwrap(), world.workspace.clone());
        let (proxy_addr, way_in) = way_in(&route).await;

        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("ca.pem"), ca.certificate().pem()).unwrap();
        std::fs::write(home.path().join("gitconfig"), "").unwrap();
        Self {
            host,
            world,
            ca,
            proxy: proxy_addr,
            home,
            _route: route,
            _root: root,
            _way_in: way_in,
        }
    }

    /// A `git` command as a person in the guest would run it: the workspace's CA is the only root
    /// it trusts, the proxy is the agent's, and no credential helper, prompt or askpass exists.
    pub(crate) fn git(&self, cwd: &Path) -> tokio::process::Command {
        let mut std_command = clean_git(self.home.path());
        std_command
            .current_dir(cwd)
            .arg("-c")
            .arg(format!("http.proxy=http://{}", self.proxy))
            .arg("-c")
            .arg(format!(
                "http.sslCAInfo={}",
                self.home.path().join("ca.pem").display()
            ))
            .arg("-c")
            .arg("credential.helper=")
            .arg("-c")
            .arg("core.askPass=")
            .arg("-c")
            .arg("user.name=guest")
            .arg("-c")
            .arg("user.email=guest@example.org");
        if cfg!(windows) {
            // Git for Windows' default TLS backend (schannel) refuses a certificate whose
            // revocation status it cannot read, and the workspace's CA publishes no list; the
            // OpenSSL backend that ships with it verifies against the CA file as given.
            std_command.arg("-c").arg("http.sslBackend=openssl");
        }
        let mut command = tokio::process::Command::from(std_command);
        command
            .env("GCM_INTERACTIVE", "never")
            .env("SSH_ASKPASS", "");
        command
    }

    /// Runs `git <args>` in `cwd`; `trace` also captures `GIT_TRACE_CURL` into the result's stderr.
    pub(crate) async fn run(&self, cwd: &Path, args: &[&str]) -> Ran {
        self.run_env(cwd, args, &[]).await
    }

    pub(crate) async fn run_env(&self, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Ran {
        let mut command = self.git(cwd);
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let output = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .unwrap();
        Ran {
            ok: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// An empty working folder under this rig's home.
    pub(crate) fn work(&self, name: &str) -> std::path::PathBuf {
        let dir = self.home.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Opens `CONNECT <authority>` through the agent (or the bridge): the stream after the `200`.
    pub(crate) async fn tunnel(&self, authority: &str) -> TcpStream {
        let mut conn = TcpStream::connect(self.proxy).await.unwrap();
        conn.write_all(
            format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            conn.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        conn
    }

    /// The connection records of the workspace, as the audit log holds them, once `count` are
    /// there.
    pub(crate) async fn records(&self, count: usize) -> Vec<Value> {
        self.records_where(|records| records.len() >= count).await
    }

    /// The connection records once `ready` accepts them (a record is written after the
    /// connection ends, so a test waits for the one it needs).
    pub(crate) async fn records_where(&self, ready: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let records: Vec<Value> = self
                .world
                .store
                .audit_lines(0, 100_000)
                .unwrap()
                .into_iter()
                .map(|(_, line)| serde_json::from_str::<Value>(&line).unwrap())
                .filter(|v| v["type"] == "connection")
                .collect();
            if ready(&records) || Instant::now() >= deadline {
                assert!(
                    ready(&records),
                    "the records the test waits for never came: {records:#?}"
                );
                return records;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Everything the audit log holds, as text: for "the token is nowhere" checks.
    pub(crate) fn audit_text(&self) -> String {
        self.world
            .store
            .audit_lines(0, 100_000)
            .unwrap()
            .into_iter()
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An HTTPS client in the guest: TLS to the workspace's CA over a tunnel, one request, the
/// response read to the end.
#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

impl Response {
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

impl GitRig {
    /// Sends `raw` (a whole HTTP/1.1 request, bytes) through a decrypted tunnel to `authority`
    /// with `sni`, trusting `roots` (the workspace's CA when `None`).
    pub(crate) async fn https_raw(
        &self,
        authority: &str,
        sni: &str,
        raw: &[u8],
    ) -> Result<Response, String> {
        let roots = [self.ca.certificate().der().clone()];
        self.https_raw_with(authority, sni, raw, &roots).await
    }

    /// As [`Self::https_raw`], trusting `roots` only.
    pub(crate) async fn https_raw_with(
        &self,
        authority: &str,
        sni: &str,
        raw: &[u8],
        roots: &[rustls::pki_types::CertificateDer<'static>],
    ) -> Result<Response, String> {
        let tunnel = self.tunnel(authority).await;
        let mut store = rustls::RootCertStore::empty();
        for root in roots {
            store.add(root.clone()).unwrap();
        }
        let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(store)
        .with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let name = rustls::pki_types::ServerName::try_from(sni.to_owned()).unwrap();
        let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config))
            .connect(name, tunnel)
            .await
            .map_err(|e| format!("{e:?}"))?;
        tls.write_all(raw).await.map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_secs(2), tls.read(&mut chunk)).await {
                Ok(Ok(0) | Err(_)) | Err(_) => break,
                Ok(Ok(n)) => {
                    bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
                    if complete(&bytes) {
                        break;
                    }
                }
            }
        }
        parse(&bytes).ok_or_else(|| format!("no response: {:?}", String::from_utf8_lossy(&bytes)))
    }

    /// `GET`/`POST`/`PUT` `path` on the host's name with `headers` and `body`.
    pub(crate) async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Response {
        let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: {NAME}\r\nConnection: close\r\n")
            .into_bytes();
        for (name, value) in headers {
            raw.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        if !body.is_empty() || method != "GET" {
            raw.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
        }
        raw.extend_from_slice(b"\r\n");
        raw.extend_from_slice(body);
        self.https_raw(&format!("{NAME}:443"), NAME, &raw)
            .await
            .unwrap()
    }
}

fn head_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Whether `bytes` hold a whole response (by `content-length`, else only when it ends the head).
fn complete(bytes: &[u8]) -> bool {
    let Some(end) = head_end(bytes) else {
        return false;
    };
    let head = String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).to_ascii_lowercase();
    let length = head.lines().find_map(|l| {
        l.strip_prefix("content-length:")
            .map(|v| v.trim().parse::<usize>().unwrap_or(0))
    });
    length.is_some_and(|n| bytes.len() >= end + 4 + n)
}

fn parse(bytes: &[u8]) -> Option<Response> {
    let end = head_end(bytes)?;
    let head = String::from_utf8_lossy(bytes.get(..end)?).into_owned();
    let mut lines = head.lines();
    let status = lines.next()?.split(' ').nth(1)?.parse().ok()?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    Some(Response {
        status,
        headers,
        body: String::from_utf8_lossy(bytes.get(end + 4..).unwrap_or_default()).into_owned(),
    })
}

/// The real agent on Unix.
#[cfg(unix)]
async fn way_in(route: &Route) -> (SocketAddr, WayIn) {
    use puddle_agent::config::Target;
    use puddle_agent::{Agent, Config};
    let config = Config {
        listen: SocketAddr::new(std::net::IpAddr::V4(LOCAL), 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        bridge: None,
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    (agent.local_addr(), agent)
}

/// Windows has no guest agent: a loopback listener that turns each connection into a stream on
/// the route, as the agent does, so the proxy side and the client are the real ones.
#[cfg(not(unix))]
async fn way_in(route: &Route) -> (SocketAddr, WayIn) {
    use futures_util::StreamExt;
    use puddle_agent_proto::tokio_yamux::Session;
    use puddle_agent_proto::yamux::client_config;
    let conn = puddle_ipc::connect(route.endpoint().path()).await.unwrap();
    let mut session = Session::new_client(conn, client_config());
    let mut control = session.control();
    tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let listener = tokio::net::TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let Ok(mut stream) = control.open_stream().await else {
                return;
            };
            tokio::spawn(async move {
                let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
            });
        }
    });
    (addr, task)
}

/// What a hook-free tool needs to read a request header: the value of `name` in a `GIT_TRACE_CURL`
/// capture, lines like `=> Send header: Authorization: Basic <redacted>`.
pub(crate) fn traced_headers(trace: &str, name: &str) -> Vec<String> {
    let prefix = format!("{}:", name.to_ascii_lowercase());
    trace
        .lines()
        .filter_map(|line| {
            line.split_once("Send header: ")
                .map(|(_, h)| h.trim().to_owned())
        })
        .filter(|h| h.to_ascii_lowercase().starts_with(&prefix))
        .collect()
}
