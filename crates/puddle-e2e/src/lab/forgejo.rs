// SPDX-License-Identifier: AGPL-3.0-or-later
//! A real Forgejo as the lab's Git host: smart HTTP over TLS and its own built-in SSH server, on
//! loopback ports, with one account, `acme`, that owns the repositories.
//!
//! Where [`GitHost`](super::GitHost) is Puddle's own stand-in (the real `git http-backend` behind a
//! small TLS server), this is a real forge answering the way a hosted one does: its pkt-line
//! answers, its token check, its refusals, its SSH server with a public key per account. It holds
//! no outside host, account or secret: the account, its token and its SSH keys are made for the
//! run, in a database in a folder removed when the fixture drops.
//!
//! Started from `PUDDLE_FORGEJO_BIN` (a Forgejo release binary; `ci/fetch-forgejo.sh` fetches the
//! pinned one). Without it `start` skips, unless `PUDDLE_REQUIRE_FORGEJO=1` makes a missing
//! Forgejo a failure (the lab workflow sets it). Forgejo publishes no Windows release (it dropped
//! Windows support), so on Windows the same variable names Gitea's Windows release, the project
//! Forgejo forked from (`ci/fetch-forgejo.sh <dir> "" windows`): its commands, `app.ini` and API are
//! the ones used here. [`Forgejo::start_on`] can put the server on a fixed port (443, so a clone URL
//! needs no port).
//!
//! The headless setup is the smallest that works: an `app.ini` with `INSTALL_LOCK`, SQLite, no
//! mail, no search index, no Actions and in-memory cache and sessions; `forgejo migrate` makes the
//! tables, `forgejo admin user create` the account, `forgejo admin user generate-access-token` its
//! token; everything after that (repositories, keys) goes through the HTTP API with that token.

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustls::pki_types::CertificateDer;
use ssh_key::private::Ed25519Keypair;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey};

use super::git_host::issue;
use super::{
    LOCAL, Prefix, READY, Requirement, StartCost, configured_or_skip, free_port, is_address_in_use,
    on_free_port, read, require_file, stop, try_answers,
};

/// The account that owns every repository of the fixture.
pub const OWNER: &str = "acme";

/// What a client needs to sign in over SSH as [`OWNER`]: made by [`Forgejo::add_generated_ssh_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshLogin {
    /// The SSH server's port on loopback.
    pub port: u16,
    /// The private half of the key the account holds (an OpenSSH file, readable by its owner only).
    pub key_path: PathBuf,
    /// The public half, an `authorized_keys` line.
    pub public_key: String,
    /// The `SHA256:` fingerprint of the host key a client should expect (the ed25519 one when the
    /// server has it).
    pub host_key_fingerprint: String,
    /// The fingerprints of every host key the server has.
    pub host_key_fingerprints: Vec<String>,
}

/// A running Forgejo. Stops when dropped.
pub struct Forgejo {
    child: Child,
    dir: tempfile::TempDir,
    name: String,
    http_port: u16,
    ssh_port: u16,
    root: CertificateDer<'static>,
    root_pem: PathBuf,
    token: String,
    password: String,
    started: StartCost,
}

impl Drop for Forgejo {
    fn drop(&mut self) {
        stop(&mut self.child);
    }
}

/// The `app.ini` of a headless instance whose files live under `dir`.
fn app_ini(dir: &Path, name: &str, http_port: u16, ssh_port: u16, secrets: &Secrets) -> String {
    // Forward slashes: Forgejo's ini reader takes a backslash as the start of an escape.
    let at = |p: &str| dir.join(p).display().to_string().replace('\\', "/");
    format!(
        "APP_NAME = lab\n\
         RUN_MODE = prod\n\
         WORK_PATH = {work}\n\
         [database]\n\
         DB_TYPE = sqlite3\n\
         PATH = {db}\n\
         [repository]\n\
         ROOT = {repos}\n\
         [server]\n\
         PROTOCOL = https\n\
         DOMAIN = {name}\n\
         ROOT_URL = https://{name}/\n\
         HTTP_ADDR = {LOCAL}\n\
         HTTP_PORT = {http_port}\n\
         CERT_FILE = {cert}\n\
         KEY_FILE = {key}\n\
         APP_DATA_PATH = {data}\n\
         START_SSH_SERVER = true\n\
         SSH_LISTEN_HOST = {LOCAL}\n\
         SSH_PORT = {ssh_port}\n\
         SSH_DOMAIN = {name}\n\
         BUILTIN_SSH_SERVER_USER = git\n\
         OFFLINE_MODE = true\n\
         DISABLE_ROUTER_LOG = true\n\
         [security]\n\
         INSTALL_LOCK = true\n\
         INTERNAL_TOKEN = {internal}\n\
         SECRET_KEY = {secret}\n\
         [oauth2]\n\
         JWT_SECRET = {jwt}\n\
         [service]\n\
         DISABLE_REGISTRATION = true\n\
         [mailer]\n\
         ENABLED = false\n\
         [session]\n\
         PROVIDER = memory\n\
         [cache]\n\
         ADAPTER = memory\n\
         [queue]\n\
         TYPE = level\n\
         [indexer]\n\
         REPO_INDEXER_ENABLED = false\n\
         [actions]\n\
         ENABLED = false\n\
         [picture]\n\
         DISABLE_GRAVATAR = true\n\
         [log]\n\
         MODE = console\n\
         LEVEL = Info\n",
        work = at(""),
        db = at("forgejo.db"),
        repos = at("repos"),
        cert = at("cert.pem"),
        key = at("key.pem"),
        data = at("data"),
        internal = secrets.internal,
        secret = secrets.secret,
        jwt = secrets.jwt,
    )
}

/// The three secrets an instance needs, made by `forgejo generate secret`.
struct Secrets {
    internal: String,
    secret: String,
    jwt: String,
}

/// One `forgejo` command against the instance in `dir`; `Err` is its output when it fails.
fn forgejo(bin: &std::ffi::OsStr, dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(bin)
        .arg("--config")
        .arg(dir.join("app.ini"))
        .arg("--work-path")
        .arg(dir)
        .args(args)
        // Forgejo writes under HOME (its own `.ssh` for authorised keys) and must not touch the
        // runner's.
        .env("HOME", dir.join("home"))
        .env("USERPROFILE", dir.join("home"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!("forgejo {}: {text}", args.join(" ")))
    }
}

/// `forgejo generate secret <kind>`, which needs no instance.
fn generate_secret(bin: &std::ffi::OsStr, kind: &str) -> String {
    let output = Command::new(bin)
        .args(["generate", "secret", kind])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "forgejo generate secret {kind}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Where an instance's HTTP API is and how to reach it as [`OWNER`].
struct Endpoint<'a> {
    root_pem: &'a Path,
    name: &'a str,
    port: u16,
    /// The `Authorization` header's value: `token <token>`, or `Basic <base64>` for the calls
    /// Forgejo answers only to the account's password.
    authorization: &'a str,
}

/// One call to the HTTP API of the instance on `port` as [`OWNER`]: the status and the body. `Err`
/// is curl's own message (a refused connection, a failed handshake).
fn call(
    at: &Endpoint<'_>,
    method: &str,
    path: &str,
    body: Option<&str>,
    max_secs: u32,
) -> Result<(u16, String), String> {
    let mut curl = Command::new("curl");
    curl.args([
        "--silent",
        "--show-error",
        "--max-time",
        &max_secs.to_string(),
    ]);
    // Windows' curl checks revocation through Schannel, which a private root has no answer for.
    if cfg!(windows) {
        curl.arg("--ssl-no-revoke");
    }
    curl.arg("--cacert")
        .arg(at.root_pem)
        .arg("--resolve")
        .arg(format!("{}:{}:{LOCAL}", at.name, at.port))
        .args(["--request", method])
        .args(["--header", "Content-Type: application/json"])
        .args(["--header", &format!("Authorization: {}", at.authorization)])
        .args(["--write-out", "\n%{http_code}"]);
    if let Some(body) = body {
        curl.args(["--data", body]);
    }
    curl.arg(format!("https://{}:{}/api/v1{path}", at.name, at.port));
    let output = curl.stdin(Stdio::null()).output().unwrap();
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    let (body, status) = text.rsplit_once('\n').unwrap();
    Ok((status.trim().parse().unwrap(), body.to_owned()))
}

/// Whether the SSH server on `port` greets with an SSH banner.
fn ssh_greets(port: u16) -> bool {
    use std::io::Read as _;
    let addr = SocketAddr::from((LOCAL, port));
    let Ok(mut conn) = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let mut banner = [0_u8; 4];
    conn.set_read_timeout(Some(Duration::from_secs(2))).is_ok()
        && conn.read_exact(&mut banner).is_ok()
        && &banner == b"SSH-"
}

/// The two ports of an instance.
struct Ports {
    http: u16,
    ssh: u16,
}

/// Waits until Forgejo itself answers on both ports: the API over TLS with the run's root (the
/// SSH one second, so a taken SSH port shows in the log before the instance is called ready). A
/// port that merely accepts proves nothing: a stranger holding the port Forgejo failed to bind
/// accepts too, and when it lets go the next connect is refused. Forgejo that failed to bind exits
/// or logs it, and either comes back as `Err` with its log.
fn await_up(
    child: &mut Child,
    root_pem: &Path,
    name: &str,
    ports: &Ports,
    token: &str,
    log: &Path,
) -> Result<(), String> {
    let log_text = || read(log);
    let http_up = || {
        let authorization = format!("token {token}");
        let at = Endpoint {
            root_pem,
            name,
            port: ports.http,
            authorization: &authorization,
        };
        call(&at, "GET", "/version", None, 2).is_ok_and(|(status, _)| status == 200)
    };
    let ssh_up = || ssh_greets(ports.ssh);
    let probes: [&dyn Fn() -> bool; 2] = [&http_up, &ssh_up];
    for up in probes {
        try_answers(
            Some(&mut *child),
            READY,
            &log_text,
            up,
            Duration::ZERO,
            // Forgejo stays up with its other listener when one bind fails.
            || is_address_in_use(&log_text()),
        )?;
    }
    Ok(())
}

/// The algorithm and `SHA256:` fingerprint of each `<type> <base64>` host key line; a line that
/// is not a key is left out.
fn host_key_fingerprints(lines: &[String]) -> Vec<(Algorithm, String)> {
    lines
        .iter()
        .filter_map(|line| PublicKey::from_openssh(line).ok())
        .map(|key| {
            (
                key.algorithm(),
                key.fingerprint(HashAlg::Sha256).to_string(),
            )
        })
        .collect()
}

/// Writes `bytes` to `path`, readable by its owner only (OpenSSH refuses a private key others can
/// read).
fn write_private(path: &Path, bytes: &[u8]) {
    use std::io::Write as _;
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path).unwrap().write_all(bytes).unwrap();
}

/// 32 random hex digits from the operating system: the account's password, made for the run.
fn random_hex() -> String {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).unwrap();
    bytes.iter().fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Checks that the API answered `201 Created` to `what`.
///
/// # Panics
/// On any other status, with Forgejo's answer.
/// The answer to a call made with the account's password. Its body can hold a token, so it has
/// no `Debug` or `Display` of its own and every failure message here carries the status and a
/// fixed text only.
struct PasswordAnswer {
    status: u16,
    body: String,
}

impl std::fmt::Debug for PasswordAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PasswordAnswer {{ status: {}, body: <hidden> }}",
            self.status
        )
    }
}

impl PasswordAnswer {
    /// # Panics
    /// When the status is not `want`; the message names `what`, the two statuses and nothing of
    /// the body.
    fn expect_status(&self, want: u16, what: &str) {
        assert!(
            self.status == want,
            "{what}: Forgejo answered {}, wanted {want} (body not logged)",
            self.status
        );
    }

    /// The `sha1` of a created token.
    ///
    /// # Panics
    /// When the status is not `201` or the body holds no token.
    fn token(&self, what: &str) -> String {
        self.expect_status(201, what);
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|json| json.get("sha1")?.as_str().map(str::to_owned))
            .unwrap_or_else(|| panic!("{what}: the answer holds no token (body not logged)"))
    }
}

fn expect_created(status: u16, answer: &str, what: &str) {
    assert_eq!(status, 201, "{what}: {answer}");
}

/// The commit a branch lookup (`what` names it) answered with, or `None` for `404`.
///
/// # Panics
/// When the answer is neither a branch nor `404`.
fn head_from_answer(status: u16, answer: &str, what: &str) -> Option<String> {
    match status {
        200 => {
            let json: serde_json::Value = serde_json::from_str(answer).unwrap();
            Some(
                json.pointer("/commit/id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap()
                    .to_owned(),
            )
        }
        404 => None,
        other => panic!("branch {what}: {other} {answer}"),
    }
}

impl Forgejo {
    /// Starts Forgejo answering to `name`, or `None` (skip) when none is configured; see the
    /// module docs. Its certificate names `name`, signed by a root made for the run.
    ///
    /// # Panics
    /// When Forgejo is required but missing, or does not start.
    #[must_use]
    pub fn start(name: &str) -> Option<Self> {
        Self::start_with(name, None)
    }

    /// Like [`Self::start`], with the HTTPS server on `http_port` instead of a free one. A port
    /// that is taken fails the start: nothing else can answer to the name.
    ///
    /// # Panics
    /// As [`Self::start`], and when `http_port` is taken.
    #[must_use]
    pub fn start_on(name: &str, http_port: u16) -> Option<Self> {
        Self::start_with(name, Some(http_port))
    }

    fn start_with(name: &str, fixed_port: Option<u16>) -> Option<Self> {
        let bin = std::env::var_os("PUDDLE_FORGEJO_BIN");
        configured_or_skip(
            bin.is_some(),
            Requirement::from_env("PUDDLE_REQUIRE_FORGEJO"),
            "PUDDLE_REQUIRE_FORGEJO",
            "set PUDDLE_FORGEJO_BIN to run against Forgejo (ci/fetch-forgejo.sh fetches the pinned release)",
        )
        .then_some(())?;
        let bin = bin?;
        require_file(&bin, "PUDDLE_FORGEJO_BIN");
        Some(Self::start_from(&bin, name, fixed_port))
    }

    /// Starts the Forgejo at `bin`, on `fixed_port` when given.
    fn start_from(bin: &std::ffi::OsStr, name: &str, fixed_port: Option<u16>) -> Self {
        Self::start_picking(bin, name, fixed_port, free_port)
    }

    /// [`Self::start_from`] with the way a free port is picked given, so a test can make the
    /// pick collide. Forgejo reads its ports from `app.ini` and takes no inherited socket, so a
    /// port cannot be held across the start; a taken one makes Forgejo exit with "address already
    /// in use", which `launch` returns as `Err`, and the start retries on fresh ports. That holds
    /// for the SSH port on a fixed HTTP port and for both ports when neither is fixed.
    fn start_picking(
        bin: &std::ffi::OsStr,
        name: &str,
        fixed_port: Option<u16>,
        pick: impl FnMut() -> u16,
    ) -> Self {
        let pick = std::cell::RefCell::new(pick);
        let next = || (pick.borrow_mut())();
        if let Some(http_port) = fixed_port {
            // A fixed HTTP port that is taken fails every attempt alike and ends in the panic.
            return on_free_port(next, |ssh_port| {
                Self::launch(bin, name, http_port, ssh_port)
            });
        }
        on_free_port(next, |http_port| Self::launch(bin, name, http_port, next()))
    }

    /// One start attempt. `Err` is Forgejo's own log when it did not come up.
    fn launch(
        bin: &std::ffi::OsStr,
        name: &str,
        http_port: u16,
        ssh_port: u16,
    ) -> Result<Self, String> {
        let begun = Instant::now();
        let prefix = Prefix::new("forgejo");
        let dir = tempfile::Builder::new()
            .prefix(prefix.as_str())
            .tempdir()
            .unwrap();
        let root_dir = dir.path();
        for sub in ["data", "repos", "home"] {
            fs::create_dir_all(root_dir.join(sub)).unwrap();
        }
        let (ca, leaf, key) = issue("Puddle lab Forgejo root", &[name.to_owned()], (2020, 2090));
        fs::write(root_dir.join("cert.pem"), leaf.pem()).unwrap();
        fs::write(root_dir.join("key.pem"), key.serialize_pem()).unwrap();
        fs::write(root_dir.join("root.pem"), ca.pem()).unwrap();
        let secrets = Secrets {
            internal: generate_secret(bin, "INTERNAL_TOKEN"),
            secret: generate_secret(bin, "SECRET_KEY"),
            jwt: generate_secret(bin, "JWT_SECRET"),
        };
        fs::write(
            root_dir.join("app.ini"),
            app_ini(root_dir, name, http_port, ssh_port, &secrets),
        )
        .unwrap();
        forgejo(bin, root_dir, &["migrate"])?;
        let password = random_hex();
        forgejo(
            bin,
            root_dir,
            &[
                "admin",
                "user",
                "create",
                "--admin",
                "--username",
                OWNER,
                "--password",
                &password,
                "--email",
                "acme@example.org",
                "--must-change-password=false",
            ],
        )?;
        let token = forgejo(
            bin,
            root_dir,
            &[
                "admin",
                "user",
                "generate-access-token",
                "--username",
                OWNER,
                "--token-name",
                "lab",
                "--scopes",
                "all",
                "--raw",
            ],
        )?
        .trim()
        .to_owned();
        let root_pem = root_dir.join("root.pem");
        let log = root_dir.join("forgejo.log");
        let mut child = Command::new(bin)
            .arg("--config")
            .arg(root_dir.join("app.ini"))
            .arg("--work-path")
            .arg(root_dir)
            .arg("web")
            .env("HOME", root_dir.join("home"))
            .env("USERPROFILE", root_dir.join("home"))
            .stdin(Stdio::null())
            .stdout(fs::File::create(&log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let ports = Ports {
            http: http_port,
            ssh: ssh_port,
        };
        if let Err(why) = await_up(&mut child, &root_pem, name, &ports, &token, &log) {
            stop(&mut child);
            return Err(why);
        }
        let started = StartCost::measure(&prefix, begun, Some(&child));
        Ok(Self {
            child,
            root: ca.der().clone(),
            root_pem,
            dir,
            name: name.to_owned(),
            http_port,
            ssh_port,
            token,
            password,
            started,
        })
    }

    /// Where the HTTPS server listens.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from((LOCAL, self.http_port))
    }

    /// Where the SSH server listens.
    #[must_use]
    pub fn ssh_addr(&self) -> SocketAddr {
        SocketAddr::from((LOCAL, self.ssh_port))
    }

    /// The root that signed the server's certificate.
    #[must_use]
    pub fn root(&self) -> &CertificateDer<'static> {
        &self.root
    }

    /// The name the certificate and the clone URLs carry.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A folder for the caller's own files, removed with the fixture.
    #[must_use]
    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }

    /// The access token of [`OWNER`], made for the run (read and write on everything the account
    /// owns).
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// What the start cost, in wall-clock and CPU time.
    #[must_use]
    pub fn start_cost(&self) -> StartCost {
        self.started
    }

    /// One call to the HTTP API as [`OWNER`]: the status and the body. `path` starts after
    /// `/api/v1`.
    ///
    /// # Panics
    /// When `curl` cannot run or its answer has no status.
    #[must_use]
    pub fn api(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        self.call_as(&format!("token {}", self.token), method, path, body)
    }

    /// [`Self::api`] signed in with the account's password, which Forgejo asks for to make or
    /// delete a token.
    ///
    /// The answer is a [`PasswordAnswer`]: its body cannot be formatted into a log or a panic
    /// message, because a call made with the password can answer with a token.
    fn api_with_password(&self, method: &str, path: &str, body: Option<&str>) -> PasswordAnswer {
        use base64::Engine as _;
        let pair = format!("{OWNER}:{}", self.password);
        let basic = base64::engine::general_purpose::STANDARD.encode(pair);
        let (status, body) = self.call_as(&format!("Basic {basic}"), method, path, body);
        PasswordAnswer { status, body }
    }

    fn call_as(
        &self,
        authorization: &str,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        // The command line holds the credential, so a failure names the path and not the command.
        call(
            &Endpoint {
                root_pem: &self.root_pem,
                name: &self.name,
                port: self.http_port,
                authorization,
            },
            method,
            path,
            body,
            60,
        )
        .unwrap_or_else(|why| panic!("curl {method} {path}: {why}"))
    }

    /// Makes another access token of [`OWNER`] named `name`, with the same reach as
    /// [`Self::token`], through the API the way a person does on the forge's settings page. A
    /// caller that revokes it ([`Self::revoke_token`]) leaves [`Self::token`] and the fixture's own
    /// calls working.
    ///
    /// # Panics
    /// When Forgejo refuses (a name already taken).
    #[must_use]
    pub fn add_token(&self, name: &str) -> String {
        let body = serde_json::json!({"name": name, "scopes": ["all"]}).to_string();
        let answer = self.api_with_password("POST", &format!("/users/{OWNER}/tokens"), Some(&body));
        answer.token(&format!("make token {name}"))
    }

    /// Deletes the token `name` of [`OWNER`] through the API: from then on Forgejo answers a
    /// request carrying it as one with a wrong password.
    ///
    /// # Panics
    /// When Forgejo refuses (no such token).
    pub fn revoke_token(&self, name: &str) {
        let answer =
            self.api_with_password("DELETE", &format!("/users/{OWNER}/tokens/{name}"), None);
        answer.expect_status(204, &format!("revoke token {name}"));
    }

    /// Makes the repository `owner/name` of [`OWNER`] (`owner` must be [`OWNER`]) on branch `main`
    /// with one commit holding `README.md`, `private` or not.
    ///
    /// # Panics
    /// When Forgejo refuses.
    pub fn add_repo(&self, repo: &str, private: bool) {
        let (owner, name) = repo.split_once('/').unwrap();
        assert_eq!(owner, OWNER, "the fixture has one account");
        let body = format!(
            r#"{{"name":"{name}","private":{private},"auto_init":true,"default_branch":"main"}}"#
        );
        let (status, answer) = self.api("POST", "/user/repos", Some(&body));
        expect_created(status, &answer, &format!("create {repo}"));
    }

    /// Adds `content` as the file `path` on the default branch of `repo` (one commit), through the
    /// contents API.
    ///
    /// # Panics
    /// When Forgejo refuses.
    pub fn add_file(&self, repo: &str, path: &str, content: &str) {
        use base64::Engine as _;
        let body = serde_json::json!({
            "content": base64::engine::general_purpose::STANDARD.encode(content),
            "message": format!("add {path}"),
        })
        .to_string();
        let (status, answer) = self.api(
            "POST",
            &format!("/repos/{repo}/contents/{path}"),
            Some(&body),
        );
        expect_created(status, &answer, &format!("add {path} to {repo}"));
    }

    /// Where the lab root's PEM file is (for a client that must trust it, such as the machine's
    /// certificate store).
    #[must_use]
    pub fn root_pem(&self) -> &Path {
        &self.root_pem
    }

    /// Adds `public_key` (an `authorized_keys` line) to [`OWNER`], so a client holding the private
    /// half signs in over SSH as `git@` the fixture's SSH address.
    ///
    /// # Panics
    /// When Forgejo refuses the key.
    pub fn add_ssh_key(&self, public_key: &str) {
        let body = serde_json::json!({"title": "lab", "key": public_key.trim()}).to_string();
        let (status, answer) = self.api("POST", "/user/keys", Some(&body));
        expect_created(status, &answer, "add key");
    }

    /// Makes an ed25519 key pair for the run, adds its public half to [`OWNER`] and writes the
    /// private half to `ssh_login_key` in [`Self::scratch`]. The key is random (from the
    /// operating system) and held by nobody else.
    ///
    /// # Panics
    /// When the key cannot be made or written, Forgejo refuses it, or the server has no host key.
    #[must_use]
    pub fn add_generated_ssh_key(&self) -> SshLogin {
        let mut seed = [0_u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
        let public_key = key.public_key().to_openssh().unwrap();
        let key_path = self.scratch().join("ssh_login_key");
        write_private(
            &key_path,
            key.to_openssh(LineEnding::LF).unwrap().as_bytes(),
        );
        self.add_ssh_key(&public_key);
        let host_keys = host_key_fingerprints(&self.ssh_host_keys());
        let primary = host_keys
            .iter()
            .find(|(algorithm, _)| *algorithm == Algorithm::Ed25519)
            .or_else(|| host_keys.first())
            .unwrap_or_else(|| panic!("the server has no SSH host key"));
        SshLogin {
            port: self.ssh_port,
            key_path,
            public_key,
            host_key_fingerprint: primary.1.clone(),
            host_key_fingerprints: host_keys.iter().map(|(_, f)| f.clone()).collect(),
        }
    }

    /// The server's SSH host keys as `<type> <base64>` lines, as `ssh-keyscan` prints them.
    #[must_use]
    pub fn ssh_host_keys(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(self.scratch().join("data/ssh")) else {
            return Vec::new();
        };
        let mut keys: Vec<String> = entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "pub"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .map(|line| {
                line.split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        keys.sort();
        keys
    }

    /// The commit `branch` of `repo` points at, or `None` when it has no such branch.
    ///
    /// # Panics
    /// When Forgejo answers anything but the branch or `404`.
    #[must_use]
    pub fn branch_head(&self, repo: &str, branch: &str) -> Option<String> {
        let (status, answer) = self.api("GET", &format!("/repos/{repo}/branches/{branch}"), None);
        head_from_answer(status, &answer, &format!("{repo}/{branch}"))
    }

    /// The server's own log so far (for a failing test to print).
    #[must_use]
    pub fn log(&self) -> String {
        read(&self.scratch().join("forgejo.log"))
    }
}

#[cfg(test)]
mod tests;
