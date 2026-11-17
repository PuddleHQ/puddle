// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company-network lab as a standing program (`puddle-lab`): the same fixtures the tests start
//! (`Squid`, [`Interceptor`], [`NtlmProxy`], [`PacServer`]), started by hand on free loopback
//! ports and left running while a person or a script drives the app against them.
//!
//! ```text
//! puddle-lab --squid-basic --interceptor --ntlm --pac --info lab.json [--ca lab-ca.pem] [--stop stop-file]
//! puddle-lab --self-test
//! ```
//!
//! Flags select the pieces; `--info` is the JSON file that lists every piece's port, the CA file
//! and the credentials (written last, in one go, so a script can wait for it). The program runs
//! until `SIGINT`, `SIGTERM` or the `--stop` file, and every piece stops with it. `--self-test`
//! starts the pieces (all of them unless some are selected), checks that each answers and exits.
//! The programs the pieces need are the fixtures' own: `PUDDLE_SQUID_BIN` or
//! `PUDDLE_SQUID_DOCKER`, and `PUDDLE_MITMDUMP_BIN`.

use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Value, json};

use super::{
    BASIC_USER, Interceptor, LOCAL, NtlmLeg, NtlmProxy, PacServer, basic_password, failover_pac,
};

/// The domain and user the NTLM proxy's usage text suggests; it accepts any.
const NTLM_NOTE: &str = "accepts any NTLM answer (any DOMAIN\\user and password)";

/// How long the self-test waits for one answer.
const ANSWER_WAIT: Duration = Duration::from_secs(10);

/// What the lab's Squid requires of a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SquidAuth {
    /// Anyone may use it.
    Open,
    /// Basic sign-in with [`BASIC_USER`] and the run's random password.
    Basic,
}

/// What the lab's TLS-intercepting proxy requires of a client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterceptorAuth {
    /// Anyone may use it.
    Open,
    /// NTLM sign-in before it opens a tunnel, as a company proxy that wants the Windows sign-in.
    Ntlm,
}

/// The proxy a Squid sends every tunnel through (its parent), as a corporate proxy chain does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parent {
    /// The TLS-intercepting proxy.
    Interceptor,
    /// The NTLM proxy.
    Ntlm,
}

/// Which pieces to start and how.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// A forward proxy (Squid), open or with Basic sign-in.
    pub squid: Option<SquidAuth>,
    /// The TLS-intercepting proxy with a root made for the run.
    pub interceptor: Option<InterceptorAuth>,
    /// The proxy that demands NTLM.
    pub ntlm: bool,
    /// The web server that hands out a PAC file.
    pub pac: bool,
    /// Names that only the lab's proxies resolve, to loopback.
    pub names: Vec<String>,
    /// Ports Squid lets requests and tunnels reach.
    pub allowed_ports: Vec<u16>,
    /// The proxy Squid chains to.
    pub squid_through: Option<Parent>,
    /// Name suffixes the PAC file sends `DIRECT`.
    pub pac_direct: Vec<String>,
}

impl Plan {
    /// Every piece, Squid with Basic sign-in.
    fn everything() -> Self {
        Self {
            squid: Some(SquidAuth::Basic),
            interceptor: Some(InterceptorAuth::Open),
            ntlm: true,
            pac: true,
            ..Self::default()
        }
    }

    fn is_empty(&self) -> bool {
        self.squid.is_none() && self.interceptor.is_none() && !self.ntlm && !self.pac
    }
}

/// What the command line asks for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Command {
    /// The pieces.
    pub plan: Plan,
    /// Where the info file goes (required to run; a self-test needs none).
    pub info: Option<PathBuf>,
    /// Where the interceptor's root certificate goes (default: next to the info file).
    pub ca: Option<PathBuf>,
    /// A file whose appearance stops the program.
    pub stop: Option<PathBuf>,
    /// Start, check every piece answers and exit.
    pub self_test: bool,
}

const USAGE: &str = "usage: puddle-lab [--squid | --squid-basic] [--interceptor | --interceptor-ntlm] [--ntlm] [--pac] [--all]\n\
    \x20      [--name HOST]... [--allow-port PORT]... [--squid-through interceptor|ntlm]\n\
    \x20      [--pac-direct SUFFIX]... --info FILE [--ca FILE] [--stop FILE]\n\
    \x20      puddle-lab --self-test [pieces]";

/// Reads the command line (without the program name).
///
/// # Errors
/// A message for a flag that is unknown, lacks its value or contradicts another.
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut cmd = Command::default();
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--squid" => set_squid(&mut cmd.plan, SquidAuth::Open)?,
            "--squid-basic" => set_squid(&mut cmd.plan, SquidAuth::Basic)?,
            "--interceptor" => set_interceptor(&mut cmd.plan, InterceptorAuth::Open)?,
            "--interceptor-ntlm" => set_interceptor(&mut cmd.plan, InterceptorAuth::Ntlm)?,
            "--ntlm" => cmd.plan.ntlm = true,
            "--pac" => cmd.plan.pac = true,
            "--all" => {
                let rest = std::mem::take(&mut cmd.plan);
                cmd.plan = Plan {
                    interceptor: rest.interceptor.or(Plan::everything().interceptor),
                    names: rest.names,
                    allowed_ports: rest.allowed_ports,
                    squid_through: rest.squid_through,
                    pac_direct: rest.pac_direct,
                    ..Plan::everything()
                };
            }
            "--name" => cmd.plan.names.push(value()?),
            "--allow-port" => {
                let port = value()?;
                cmd.plan.allowed_ports.push(
                    port.parse()
                        .map_err(|e| format!("--allow-port {port}: {e}"))?,
                );
            }
            "--squid-through" => {
                cmd.plan.squid_through = Some(match value()?.as_str() {
                    "interceptor" => Parent::Interceptor,
                    "ntlm" => Parent::Ntlm,
                    other => {
                        return Err(format!("--squid-through {other}: use interceptor or ntlm"));
                    }
                });
            }
            "--pac-direct" => cmd.plan.pac_direct.push(value()?),
            "--info" => cmd.info = Some(PathBuf::from(value()?)),
            "--ca" => cmd.ca = Some(PathBuf::from(value()?)),
            "--stop" => cmd.stop = Some(PathBuf::from(value()?)),
            "--self-test" => cmd.self_test = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if cmd.plan.is_empty() {
        if !cmd.self_test {
            return Err(
                "select at least one piece (--squid, --interceptor, --ntlm, --pac or --all)".into(),
            );
        }
        cmd.plan = Plan {
            interceptor: cmd.plan.interceptor.or(Plan::everything().interceptor),
            names: cmd.plan.names,
            allowed_ports: cmd.plan.allowed_ports,
            squid_through: cmd.plan.squid_through,
            pac_direct: cmd.plan.pac_direct,
            ..Plan::everything()
        };
    }
    if cmd.plan.allowed_ports.is_empty() {
        cmd.plan.allowed_ports = vec![80, 443];
    }
    if !cmd.self_test && cmd.info.is_none() {
        return Err("--info is required".into());
    }
    match (cmd.plan.squid, cmd.plan.squid_through) {
        (None, Some(_)) => return Err("--squid-through needs --squid or --squid-basic".into()),
        (_, Some(Parent::Interceptor)) if cmd.plan.interceptor.is_none() => {
            return Err("--squid-through interceptor needs --interceptor".into());
        }
        (_, Some(Parent::Ntlm)) if !cmd.plan.ntlm => {
            return Err("--squid-through ntlm needs --ntlm".into());
        }
        _ => {}
    }
    if cmd.plan.pac && cmd.plan.squid.is_none() && cmd.plan.interceptor.is_none() && !cmd.plan.ntlm
    {
        return Err("--pac needs a proxy to point at (--squid, --interceptor or --ntlm)".into());
    }
    Ok(cmd)
}

fn set_interceptor(plan: &mut Plan, auth: InterceptorAuth) -> Result<(), String> {
    if plan.interceptor.is_some_and(|set| set != auth) {
        return Err("--interceptor and --interceptor-ntlm exclude each other".into());
    }
    plan.interceptor = Some(auth);
    Ok(())
}

fn set_squid(plan: &mut Plan, auth: SquidAuth) -> Result<(), String> {
    if plan.squid.is_some_and(|set| set != auth) {
        return Err("--squid and --squid-basic exclude each other".into());
    }
    plan.squid = Some(auth);
    Ok(())
}

/// The running pieces; each stops when this is dropped.
pub struct Lab {
    plan: Plan,
    ca: Option<PathBuf>,
    #[cfg(unix)]
    squid: Option<super::Squid>,
    interceptor: Option<Interceptor>,
    ntlm: Option<NtlmProxy>,
    pac: Option<PacServer>,
}

impl Lab {
    /// Starts the pieces of `plan` and writes the interceptor's root to `ca`.
    ///
    /// # Errors
    /// A message naming the piece whose program is not configured, or the file that cannot be
    /// written. A program that is configured but does not start panics, as the fixtures do.
    pub fn start(plan: &Plan, ca: &Path) -> Result<Self, String> {
        let names: Vec<&str> = plan.names.iter().map(String::as_str).collect();
        let interceptor = if let Some(auth) = plan.interceptor {
            let started = match auth {
                InterceptorAuth::Ntlm => Interceptor::start_signing_in(&names),
                InterceptorAuth::Open => Interceptor::start(&names),
            }
            .ok_or("the interceptor needs PUDDLE_MITMDUMP_BIN (a mitmdump binary)")?;
            std::fs::write(ca, started.root().pem())
                .map_err(|e| format!("cannot write {}: {e}", ca.display()))?;
            Some(started)
        } else {
            None
        };
        let ntlm = if plan.ntlm {
            Some(NtlmProxy::start(&names).ok_or("the NTLM proxy needs PUDDLE_MITMDUMP_BIN")?)
        } else {
            None
        };
        #[cfg(unix)]
        let squid = start_squid(plan, interceptor.as_ref(), ntlm.as_ref())?;
        #[cfg(not(unix))]
        if plan.squid.is_some() {
            return Err("Squid is not available on this platform".into());
        }
        let mut lab = Self {
            plan: plan.clone(),
            ca: plan.interceptor.map(|_| ca.to_path_buf()),
            #[cfg(unix)]
            squid,
            interceptor,
            ntlm,
            pac: None,
        };
        if plan.pac {
            let direct: Vec<&str> = plan.pac_direct.iter().map(String::as_str).collect();
            let hops = lab.hops();
            let hops: Vec<&str> = hops.iter().map(String::as_str).collect();
            lab.pac = Some(PacServer::start(&failover_pac(&direct, &hops)));
        }
        Ok(lab)
    }

    /// `host:port` of each proxy, the order a PAC file tries them: Squid, interceptor, NTLM.
    fn hops(&self) -> Vec<String> {
        [self.squid_port(), self.interceptor_port(), self.ntlm_port()]
            .into_iter()
            .flatten()
            .map(|port| format!("{LOCAL}:{port}"))
            .collect()
    }

    #[cfg(unix)]
    fn squid_port(&self) -> Option<u16> {
        self.squid.as_ref().map(super::Squid::port)
    }

    #[cfg(not(unix))]
    #[expect(clippy::unused_self, reason = "the same call on every platform")]
    fn squid_port(&self) -> Option<u16> {
        None
    }

    fn interceptor_port(&self) -> Option<u16> {
        self.interceptor.as_ref().map(Interceptor::port)
    }

    fn ntlm_port(&self) -> Option<u16> {
        self.ntlm.as_ref().map(NtlmProxy::port)
    }

    /// What the info file says: every piece's port, the CA file, the credentials.
    #[must_use]
    pub fn info(&self) -> Value {
        let proxy = |port: u16| format!("http://{LOCAL}:{port}");
        let mut info = serde_json::Map::new();
        info.insert("names".into(), json!(self.plan.names));
        if let Some(port) = self.squid_port() {
            let basic = self.plan.squid == Some(SquidAuth::Basic);
            info.insert(
                "squid".into(),
                json!({
                    "port": port,
                    "proxy_url": proxy(port),
                    "auth": if basic { "basic" } else { "none" },
                    "user": basic.then_some(BASIC_USER),
                    "password": basic.then(basic_password),
                    "allowed_ports": self.plan.allowed_ports,
                    "through": self.plan.squid_through.map(|p| format!("{p:?}").to_lowercase()),
                }),
            );
        }
        if let Some(port) = self.interceptor_port() {
            info.insert(
                "interceptor".into(),
                json!({
                    "port": port,
                    "proxy_url": proxy(port),
                    "ca": self.ca,
                    "sign_in": (self.plan.interceptor == Some(InterceptorAuth::Ntlm)).then_some("ntlm"),
                    "log": self.interceptor.as_ref().map(Interceptor::log_path),
                }),
            );
        }
        if let Some(port) = self.ntlm_port() {
            info.insert(
                "ntlm".into(),
                json!({
                    "port": port,
                    "proxy_url": proxy(port),
                    "accepts": NTLM_NOTE,
                    "log": self.ntlm.as_ref().map(NtlmProxy::log_path),
                }),
            );
        }
        if let Some(pac) = &self.pac {
            info.insert(
                "pac".into(),
                json!({ "url": pac.url(), "proxies": self.hops() }),
            );
        }
        Value::Object(info)
    }

    /// Asks every piece a question only it answers and returns one line per piece: `Ok` is what
    /// the piece did, `Err` what was wrong. `origin` is a port that answers plain HTTP with `200`
    /// and that Squid allows.
    #[must_use]
    pub fn check(&self, origin: u16) -> Vec<Result<String, String>> {
        let mut lines = Vec::new();
        #[cfg(unix)]
        if let Some(squid) = &self.squid {
            lines.push(check_squid(squid.port(), self.plan.squid, origin));
        }
        if let (Some(port), Some(ca), Some(interceptor)) =
            (self.interceptor_port(), &self.ca, &self.interceptor)
        {
            lines.push(if self.plan.interceptor == Some(InterceptorAuth::Ntlm) {
                check_signing_in_interceptor(port, ca, interceptor)
            } else {
                check_interceptor(port, ca, origin)
            });
        }
        if let (Some(port), Some(ntlm)) = (self.ntlm_port(), &self.ntlm) {
            lines.push(check_ntlm(port, ntlm, origin));
        }
        if let Some(pac) = &self.pac {
            lines.push(check_pac(pac, &self.hops()));
        }
        lines
    }
}

#[cfg(unix)]
fn start_squid(
    plan: &Plan,
    interceptor: Option<&Interceptor>,
    ntlm: Option<&NtlmProxy>,
) -> Result<Option<super::Squid>, String> {
    use super::{Squid, SquidOptions};
    let Some(auth) = plan.squid else {
        return Ok(None);
    };
    let mut options = match auth {
        SquidAuth::Open => SquidOptions::open(&plan.allowed_ports),
        SquidAuth::Basic => SquidOptions::basic(&plan.allowed_ports),
    };
    options.names.extend(plan.names.iter().cloned());
    options.parent_port = match plan.squid_through {
        Some(Parent::Interceptor) => interceptor.map(Interceptor::port),
        Some(Parent::Ntlm) => ntlm.map(NtlmProxy::port),
        None => None,
    };
    Squid::start(&options)
        .map(Some)
        .ok_or_else(|| "Squid needs PUDDLE_SQUID_BIN or PUDDLE_SQUID_DOCKER".into())
}

/// One request and its whole answer, from a server that closes after answering.
fn http(port: u16, request: &str) -> Result<String, String> {
    let addr = SocketAddr::from((LOCAL, port));
    let mut conn =
        TcpStream::connect_timeout(&addr, ANSWER_WAIT).map_err(|e| format!("{addr}: {e}"))?;
    conn.set_read_timeout(Some(ANSWER_WAIT))
        .map_err(|e| e.to_string())?;
    conn.write_all(request.as_bytes())
        .map_err(|e| format!("{addr}: {e}"))?;
    let mut answer = Vec::new();
    conn.read_to_end(&mut answer)
        .map_err(|e| format!("{addr}: {e}"))?;
    Ok(String::from_utf8_lossy(&answer).into_owned())
}

/// A request through a proxy for `http://127.0.0.1:<origin>/` (`extra` is more header lines).
fn via_proxy(proxy: u16, origin: u16, extra: &str) -> Result<String, String> {
    http(
        proxy,
        &format!(
            "GET http://{LOCAL}:{origin}/ HTTP/1.1\r\nHost: {LOCAL}:{origin}\r\n{extra}Connection: close\r\n\r\n"
        ),
    )
}

fn status(answer: &str) -> &str {
    answer.lines().next().unwrap_or("(nothing)")
}

fn expect_status(answer: &str, code: &str, what: &str) -> Result<(), String> {
    if status(answer).split_whitespace().nth(1) == Some(code) {
        Ok(())
    } else {
        Err(format!("{what}: wanted {code}, got {}", status(answer)))
    }
}

#[cfg(unix)]
fn check_squid(port: u16, auth: Option<SquidAuth>, origin: u16) -> Result<String, String> {
    if auth == Some(SquidAuth::Basic) {
        let anonymous = via_proxy(port, origin, "")?;
        expect_status(&anonymous, "407", "squid without credentials")?;
        let login = base64_basic(&format!("{BASIC_USER}:{}", basic_password()));
        let header = format!("Proxy-Authorization: Basic {login}\r\n");
        let signed_in = via_proxy(port, origin, &header)?;
        expect_status(&signed_in, "200", "squid with the Basic login")?;
        return Ok(format!("squid on {port}: 407 without a login, 200 with it"));
    }
    expect_status(&via_proxy(port, origin, "")?, "200", "squid")?;
    Ok(format!("squid on {port}: forwards a request"))
}

#[cfg(unix)]
fn base64_basic(text: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(text)
}

fn check_interceptor(port: u16, ca: &Path, origin: u16) -> Result<String, String> {
    expect_status(&via_proxy(port, origin, "")?, "200", "interceptor")?;
    check_ca_file(ca)?;
    Ok(format!(
        "interceptor on {port}: forwards a request, root in {}",
        ca.display()
    ))
}

fn check_ca_file(ca: &Path) -> Result<(), String> {
    let pem = std::fs::read_to_string(ca).map_err(|e| format!("CA file {}: {e}", ca.display()))?;
    if pem.contains("BEGIN CERTIFICATE") {
        Ok(())
    } else {
        Err(format!("CA file {} holds no certificate", ca.display()))
    }
}

/// The interceptor that wants NTLM: a tunnel is refused with an NTLM demand (and the demand is
/// logged), and the root is written.
fn check_signing_in_interceptor(
    port: u16,
    ca: &Path,
    interceptor: &Interceptor,
) -> Result<String, String> {
    let head = connect_head(port, port)?;
    expect_status(&head, "407", "interceptor that wants NTLM")?;
    ntlm_offered(&head)?;
    puddle_test_kit::eventually_blocking("the NTLM demand is logged", ANSWER_WAIT, || {
        interceptor
            .legs()
            .iter()
            .any(|leg| matches!(leg, NtlmLeg::Demand { .. }))
    });
    check_ca_file(ca)?;
    Ok(format!(
        "interceptor on {port}: demands NTLM with a 407 before a tunnel, root in {}",
        ca.display()
    ))
}

/// Whether a `407` head offers NTLM.
fn ntlm_offered(head: &str) -> Result<(), String> {
    if head
        .to_ascii_lowercase()
        .contains("proxy-authenticate: ntlm")
    {
        return Ok(());
    }
    Err(format!("NTLM proxy: the 407 does not offer NTLM\n{head}"))
}

/// The head of the answer to a CONNECT: its `407` stays open, so the body is not read.
fn connect_head(port: u16, origin: u16) -> Result<String, String> {
    let addr = SocketAddr::from((LOCAL, port));
    let mut conn =
        TcpStream::connect_timeout(&addr, ANSWER_WAIT).map_err(|e| format!("{addr}: {e}"))?;
    conn.set_read_timeout(Some(ANSWER_WAIT))
        .map_err(|e| e.to_string())?;
    let request = format!("CONNECT {LOCAL}:{origin} HTTP/1.1\r\nHost: {LOCAL}:{origin}\r\n\r\n");
    conn.write_all(request.as_bytes())
        .map_err(|e| format!("{addr}: {e}"))?;
    let mut reader = std::io::BufReader::new(conn);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|e| format!("{addr}: {e}"))?;
        head.push_str(&line);
        if read == 0 || line == "\r\n" {
            break;
        }
    }
    Ok(head)
}

fn check_ntlm(port: u16, ntlm: &NtlmProxy, origin: u16) -> Result<String, String> {
    // A CONNECT is what the proxy's handshake is for (and what it logs).
    let head = connect_head(port, origin)?;
    expect_status(&head, "407", "NTLM proxy")?;
    ntlm_offered(&head)?;
    // The proxy logs the demand as it answers; the log file can trail by a moment.
    puddle_test_kit::eventually_blocking("the NTLM demand is logged", ANSWER_WAIT, || {
        ntlm.legs()
            .iter()
            .any(|leg| matches!(leg, NtlmLeg::Demand { .. }))
    });
    Ok(format!("NTLM proxy on {port}: demands NTLM with a 407"))
}

fn check_pac(pac: &PacServer, hops: &[String]) -> Result<String, String> {
    let url = pac.url();
    let (host, path) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(|| format!("PAC url {url} is not http://host:port/path"))?;
    let port = host
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("PAC url {url} has no port"))?;
    let request = format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let answer = http(port, &request)?;
    expect_status(&answer, "200", "PAC server")?;
    if !answer.contains("FindProxyForURL") || !hops.iter().all(|hop| answer.contains(hop)) {
        return Err(format!("PAC server: the script is wrong\n{answer}"));
    }
    Ok(format!("PAC server: {url} names {}", hops.join(", ")))
}

/// A server that answers every request with `200`, for the self-test; its port is the one the
/// proxies are asked to reach. Returns the port; the thread ends with the process.
fn origin_server() -> Result<u16, String> {
    let listener = TcpListener::bind((LOCAL, 0)).map_err(|e| e.to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    Ok(port)
}

/// The self-test: starts the pieces, checks each, prints one line per piece.
///
/// # Errors
/// The failed checks, or the start that did not work.
pub fn self_test(plan: &Plan) -> Result<Vec<String>, String> {
    let origin = origin_server()?;
    let mut plan = plan.clone();
    plan.allowed_ports.push(origin);
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let lab = Lab::start(&plan, &dir.path().join("lab-ca.pem"))?;
    sort_checks(lab.check(origin))
}

/// The checks that passed, or every failure joined when any failed.
fn sort_checks(checks: Vec<Result<String, String>>) -> Result<Vec<String>, String> {
    let (ok, bad): (Vec<_>, Vec<_>) = checks.into_iter().partition(Result::is_ok);
    let bad: Vec<String> = bad.into_iter().filter_map(Result::err).collect();
    if bad.is_empty() {
        Ok(ok.into_iter().filter_map(Result::ok).collect())
    } else {
        Err(bad.join("\n"))
    }
}

/// Runs until a signal or the stop file; `Err` is a message for the user.
fn run(cmd: &Command) -> Result<(), String> {
    let info_path = cmd.info.as_deref().ok_or("--info is required")?;
    let ca = cmd
        .ca
        .clone()
        .unwrap_or_else(|| info_path.with_extension("ca.pem"));
    let lab = Lab::start(&cmd.plan, &ca)?;
    // Written last and in one go: a script waits for the file.
    let part = info_path.with_extension("part");
    std::fs::write(&part, lab.info().to_string())
        .and_then(|()| std::fs::rename(&part, info_path))
        .map_err(|e| format!("cannot write {}: {e}", info_path.display()))?;
    eprintln!("puddle-lab: ready, info in {}", info_path.display());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(shutdown(cmd.stop.as_deref()))?;
    eprintln!("puddle-lab: stopping");
    Ok(())
}

/// Returns when the process is asked to stop: `SIGINT`, `SIGTERM` or the stop file appearing.
///
/// # Errors
/// When the `SIGTERM` handler cannot be installed.
async fn shutdown(stop: Option<&Path>) -> Result<(), String> {
    let stop_file = async {
        let Some(stop) = stop else {
            return std::future::pending::<()>().await;
        };
        // flake-ok: a program waiting for the stop file its script writes, not a test waiting for a condition.
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        while !stop.exists() {
            tick.tick().await;
        }
    };
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|e| format!("cannot listen for SIGTERM: {e}"))?;
    #[cfg(unix)]
    let terminate = term.recv();
    #[cfg(not(unix))]
    let terminate = std::future::pending::<Option<()>>();
    tokio::select! {
        () = stop_file => {}
        _ = terminate => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    Ok(())
}

/// The program: parse, then run or self-test. The exit code is 0 for a clean run, 1 for a failed
/// self-test or start, 2 for a bad command line.
#[must_use]
pub fn main(args: impl Iterator<Item = String>) -> ExitCode {
    let cmd = match parse(args) {
        Ok(cmd) => cmd,
        Err(message) => {
            eprintln!("puddle-lab: {message}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = if cmd.self_test {
        self_test(&cmd.plan).map(|lines| {
            for line in lines {
                eprintln!("puddle-lab: ok: {line}");
            }
        })
    } else {
        run(&cmd)
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("puddle-lab: {message}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests;
