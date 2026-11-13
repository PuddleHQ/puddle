// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-agent connect <host> <port> [<name>]`: the command `ssh` runs as its `ProxyCommand`
//! (`%h %p %n`) in a sandbox.
//!
//! `ssh` talks to it over stdin and stdout, and the command carries that connection to the proxy
//! through the agent's listener as a `CONNECT`. SSH is not supported yet: the command reads the
//! client's identification line first, and when it is SSH (`SSH-2.0-...`, on any port) it says so
//! in the request ([`puddle_agent_proto::ssh::PROTOCOL_HEADER`]) and sends the line along, so the
//! proxy refuses it at once without asking about the destination, whatever it is. The refusal
//! goes to stderr, which `ssh` passes on to its own, so the user, `git` and tools that run `ssh`
//! show it.
//!
//! Anything that is not SSH is carried as it is: the command is a plain `CONNECT` helper for a
//! client that is not.
//!
//! What is inside the workspace is not outbound and is not sent to the proxy: the command
//! looks the destination up itself and, when every address it names is loopback or inside a
//! `--local-net` (the container bridge), dials that address directly and carries the connection.
//! It is decided by the address, never by the name: a DNS name that starts with `127.` resolves
//! to whatever its zone says and goes to the proxy unless that is local. The address that was
//! checked is the one dialled.
//!
//! `<name>` (the host name as typed, before `HostName`) is accepted and unused until SSH is.
//! `ssh` ignores the exit status of its `ProxyCommand`: only what the command printed matters.

use std::fmt::Write as _;
use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::time::Duration;

use puddle_agent_proto::origin::{CONTAINER, ORIGIN_HEADER};
use puddle_agent_proto::ssh::{BANNER_MIN, PROTOCOL_HEADER, SSH, is_banner};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::TcpStream;
use tokio::time::Instant;

use crate::config::Config;

/// Usage text.
pub const USAGE: &str =
    "usage: puddle-agent connect [--local-net <address>/<bits>]... <host> <port> [<name>]";

/// Longest response head read from the proxy.
const MAX_HEAD: u64 = 16 * 1024;

/// Most of a refusal's body that is shown.
const MAX_BODY: usize = 4 * 1024;

/// The destination `ssh` asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    host: String,
    port: u16,
    local: Vec<LocalNet>,
}

/// An IPv4 network whose addresses are inside the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalNet {
    base: u32,
    mask: u32,
}

impl LocalNet {
    fn parse(text: &str) -> Option<Self> {
        let (address, bits) = text.split_once('/')?;
        let bits = bits.parse::<u32>().ok().filter(|b| (1..=32).contains(b))?;
        let mask = u32::MAX << (32 - bits);
        Some(Self {
            base: u32::from(address.parse::<Ipv4Addr>().ok()?) & mask,
            mask,
        })
    }

    fn contains(self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mask == self.base
    }
}

impl Request {
    /// Whether `ip` is inside the workspace: loopback (0.0.0.0 and :: reach it too) or inside a
    /// `--local-net`.
    fn is_local(&self, ip: IpAddr) -> bool {
        match ip.to_canonical() {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_unspecified() || self.local.iter().any(|n| n.contains(v4))
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        }
    }
}

impl Request {
    /// Reads `[--local-net <address>/<bits>]... <host> <port> [<name>]`.
    ///
    /// # Errors
    ///
    /// The usage text, or what is wrong with the host or port.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Self, String> {
        let mut args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let mut local = Vec::new();
        while let ["--local-net", net, ..] = args.as_slice() {
            local
                .push(LocalNet::parse(net).ok_or_else(|| format!("bad network {net:?}\n{USAGE}"))?);
            args.drain(..2);
        }
        let (host, port) = match args.as_slice() {
            [host, port] | [host, port, _] => (*host, *port),
            _ => return Err(USAGE.to_owned()),
        };
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or_else(|| format!("bad port {port:?}\n{USAGE}"))?;
        // `ssh user@[2001:db8::1]` hands the brackets on.
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .filter(|h| h.parse::<Ipv6Addr>().is_ok())
            .unwrap_or(host);
        let plain =
            |b: u8| b.is_ascii_graphic() && !matches!(b, b'/' | b'@' | b'?' | b'#' | b'[' | b']');
        if host.is_empty() || host.len() > 253 || !host.bytes().all(plain) {
            return Err(format!(
                "bad host {:?}\n{USAGE}",
                host.chars().take(64).collect::<String>()
            ));
        }
        Ok(Self {
            host: host.to_owned(),
            port,
            local,
        })
    }

    /// `host:port` as a `CONNECT` target (an IPv6 address in brackets).
    fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// How a destination name becomes addresses: `None` when it can't be looked up in time.
pub type Lookup =
    fn(String, u16, Duration) -> Pin<Box<dyn Future<Output = Option<Vec<IpAddr>>> + Send>>;

/// The system resolver, giving up after `limit`.
pub fn system_lookup(
    host: String,
    port: u16,
    limit: Duration,
) -> Pin<Box<dyn Future<Output = Option<Vec<IpAddr>>> + Send>> {
    Box::pin(async move { within(limit, tokio::net::lookup_host((host.as_str(), port))).await })
}

/// The addresses `resolving` yields, or `None` when it fails or takes longer than `limit`.
async fn within<A: Iterator<Item = SocketAddr>>(
    limit: Duration,
    resolving: impl Future<Output = io::Result<A>>,
) -> Option<Vec<IpAddr>> {
    let found = tokio::time::timeout(limit, resolving).await.ok()?.ok()?;
    Some(found.map(|a| a.ip()).collect())
}

/// Where the proxy is and how long each step may take.
#[derive(Debug, Clone, Copy)]
pub struct Settings {
    /// The agent's proxy listener.
    pub proxy: SocketAddr,
    /// How long to wait for the client's first bytes before treating it as no SSH client.
    pub first_bytes: Duration,
    /// How long the proxy may take to answer (it connects to the destination first).
    pub answer: Duration,
    /// How long to wait for the destination's name to resolve; a name that doesn't in time is
    /// not local.
    pub lookup_limit: Duration,
    /// Resolves destination names.
    pub lookup: Lookup,
}

impl Settings {
    /// The agent's own settings (`PUDDLE_AGENT_LISTEN`), with the usual timeouts.
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        let mut proxy = config.listen;
        if proxy.ip().is_unspecified() {
            proxy.set_ip(Ipv4Addr::LOCALHOST.into());
        }
        Self {
            proxy,
            first_bytes: Duration::from_secs(2),
            answer: Duration::from_secs(60),
            lookup_limit: Duration::from_secs(2),
            lookup: system_lookup,
        }
    }
}

/// How the command ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The connection was carried until either side closed it.
    Done,
    /// It was refused or could not be made; the reason is on stderr.
    Failed,
}

impl Exit {
    /// The process exit status: 255, as `ssh` itself uses for a failed connection.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Done => 0,
            Self::Failed => 255,
        }
    }
}

/// Carries `stdin` and `stdout` to `request`'s destination through the proxy, or says on `stderr`
/// why it can't.
pub async fn run<I, O, E>(
    settings: &Settings,
    request: &Request,
    mut stdin: I,
    stdout: O,
    stderr: &mut E,
) -> Exit
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    let mut stdout = stdout;
    if let Some(addresses) = inside_the_workspace(settings, request).await {
        return direct(&addresses, request.port, stdin, &mut stdout, stderr).await;
    }
    let first = peek(&mut stdin, settings.first_bytes).await;
    match open(settings, request, &first).await {
        Ok(Opened::Tunnel { conn, rest }) => {
            carry_until_the_server_closes(conn, &rest, stdin, stdout).await
        }
        Ok(Opened::Refused(text)) => {
            say(stderr, &text).await;
            Exit::Failed
        }
        Err(err) => {
            let authority = request.authority();
            let proxy = settings.proxy;
            say(
                stderr,
                &format!("puddle-agent connect: {authority} through {proxy}: {err}\n"),
            )
            .await;
            Exit::Failed
        }
    }
}

/// The destination's addresses when every one of them is inside the workspace, else `None`.
async fn inside_the_workspace(settings: &Settings, request: &Request) -> Option<Vec<IpAddr>> {
    let addresses = if let Ok(ip) = request.host.parse::<IpAddr>() {
        vec![ip]
    } else {
        let found =
            (settings.lookup)(request.host.clone(), request.port, settings.lookup_limit).await;
        let Some(found) = found else {
            tracing::debug!(host = %request.host, "no answer from the resolver in time; using the proxy");
            return None;
        };
        found
    };
    if addresses.is_empty() || !addresses.iter().all(|ip| request.is_local(*ip)) {
        tracing::debug!(host = %request.host, ?addresses, "not every address is local; using the proxy");
        return None;
    }
    Some(addresses)
}

/// Carries the connection to one of `addresses`, which were checked, without the proxy.
async fn direct<I, O, E>(
    addresses: &[IpAddr],
    port: u16,
    stdin: I,
    stdout: &mut O,
    stderr: &mut E,
) -> Exit
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    let mut failure = String::new();
    for ip in addresses {
        // An IPv4-mapped IPv6 address (::ffff:127.0.0.1) is local by `is_local`; dial its IPv4
        // form, since Windows refuses to connect to the mapped one (os error 10049).
        match TcpStream::connect(SocketAddr::new(ip.to_canonical(), port)).await {
            Ok(conn) => return carry_until_the_server_closes(conn, &[], stdin, stdout).await,
            Err(err) => failure = format!("puddle-agent connect: {ip} port {port}: {err}\n"),
        }
    }
    say(stderr, &failure).await;
    Exit::Failed
}

/// Carries both directions until the server is done. The client's own end is no signal: `stdout`
/// is a pipe that stays open until this process exits, so a client waiting for the server to
/// close would never close its side first. `ahead` is what the server already sent: it goes to
/// `stdout` before anything else.
async fn carry_until_the_server_closes<I, O>(
    conn: TcpStream,
    ahead: &[u8],
    mut stdin: I,
    mut stdout: O,
) -> Exit
where
    I: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
{
    let (mut from_server, mut to_server) = conn.into_split();
    let down = async {
        // `copy` flushes only what it wrote itself, and the process's stdout is line-buffered:
        // without this flush an SSH server's KEXINIT that came in behind its banner stays here
        // while the client waits for it.
        stdout.write_all(ahead).await?;
        stdout.flush().await?;
        tokio::io::copy(&mut from_server, &mut stdout).await?;
        stdout.flush().await
    };
    let up = async {
        tokio::io::copy(&mut stdin, &mut to_server).await?;
        to_server.shutdown().await
    };
    tokio::pin!(down, up);
    let ended: io::Result<()> = async {
        tokio::select! {
            down = &mut down => down,
            up = &mut up => {
                up?;
                down.await
            }
        }
    }
    .await;
    match ended {
        Ok(()) => Exit::Done,
        Err(err) => {
            tracing::debug!(error = %err, "connection ended with an error");
            Exit::Failed
        }
    }
}

/// What the proxy answered.
enum Opened {
    /// `200`: the tunnel is open; `rest` is what the proxy sent behind its answer.
    Tunnel { conn: TcpStream, rest: Vec<u8> },
    /// Anything else: the proxy's own text.
    Refused(String),
}

/// Reads what the client sends first: up to [`BANNER_MIN`] bytes, as long as they arrive within
/// `limit`. An empty answer is a client that waits for the server (or a closed stdin).
pub(crate) async fn peek<I: AsyncRead + Unpin>(stdin: &mut I, limit: Duration) -> Vec<u8> {
    let deadline = Instant::now() + limit;
    let mut seen = Vec::new();
    let mut chunk = [0u8; 256];
    while seen.len() < BANNER_MIN {
        match tokio::time::timeout_at(deadline, stdin.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => seen.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            _ => break,
        }
    }
    seen
}

/// A `CONNECT` for `authority` with the client's `first` bytes right behind it, the header that
/// says "SSH" when they are an SSH identification line, so the proxy decides before it asks about
/// the destination, and the one that says the client is in a container when `from_container`.
pub(crate) fn connect_request(authority: &str, first: &[u8], from_container: bool) -> Vec<u8> {
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if is_banner(first) {
        let _ = write!(head, "{PROTOCOL_HEADER}: {SSH}\r\n");
    }
    if from_container {
        let _ = write!(head, "{ORIGIN_HEADER}: {CONTAINER}\r\n");
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(first);
    out
}

/// Sends the `CONNECT` (with the client's first bytes right behind it) and reads the answer.
async fn open(settings: &Settings, request: &Request, first: &[u8]) -> io::Result<Opened> {
    let out = connect_request(&request.authority(), first, false);
    let mut reader = BufReader::new(TcpStream::connect(settings.proxy).await?);
    reader.get_mut().write_all(&out).await?;
    let (status, body) = tokio::time::timeout(settings.answer, read_answer(&mut reader))
        .await
        .map_err(|_| io::ErrorKind::TimedOut)??;
    if status == 200 {
        let rest = reader.buffer().to_vec();
        return Ok(Opened::Tunnel {
            conn: reader.into_inner(),
            rest,
        });
    }
    let text = String::from_utf8_lossy(&body).trim_end().to_owned();
    Ok(Opened::Refused(if text.is_empty() {
        format!("puddle: the proxy answered {status}\n")
    } else {
        format!("{text}\n")
    }))
}

/// The status of the proxy's answer and, for anything but `200`, its body.
pub(crate) async fn read_answer<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> io::Result<(u16, Vec<u8>)> {
    let mut head = Vec::new();
    let mut limited = reader.take(MAX_HEAD);
    loop {
        let before = head.len();
        limited.read_until(b'\n', &mut head).await?;
        if head.len() == before {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the proxy closed the connection",
            ));
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "the proxy's answer is not HTTP")
        })?;
    if status == 200 {
        return Ok((status, Vec::new()));
    }
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .map_or(MAX_BODY, |n| n.min(MAX_BODY));
    let mut body = Vec::new();
    reader.take(length as u64).read_to_end(&mut body).await?;
    Ok((status, body))
}

/// Writes `text` to `stderr`; a stderr that is gone is nobody to tell.
async fn say<E: AsyncWrite + Unpin>(stderr: &mut E, text: &str) {
    let _ = stderr.write_all(text.as_bytes()).await;
    let _ = stderr.flush().await;
}

#[cfg(test)]
mod tests;
