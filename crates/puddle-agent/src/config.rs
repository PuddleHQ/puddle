// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent's settings, from environment variables (the boot hook sets them).
//!
//! | Variable | Default | |
//! |---|---|---|
//! | `PUDDLE_AGENT_LISTEN` | `127.0.0.1:3128` | proxy listener (the sandbox's own processes) |
//! | `PUDDLE_AGENT_BRIDGE` | `172.17.0.1:3128` | the Docker bridge address nested containers use as their proxy: the agent listens there only while a bridge interface owns that address; `off` disables it |
//! | `PUDDLE_AGENT_BRIDGE_POLL_MS` | `200` | how often the bridge is looked for |
//! | `PUDDLE_AGENT_TARGET` | `vsock://2:5000` | the host: `vsock://<cid>:<port>`, or `unix:///path` for tests without a VM |
//! | `PUDDLE_AGENT_MUX` | `4` | yamux sessions (vsock connections) to spread streams over, 1–64 |
//! | `PUDDLE_AGENT_VSOCK_BUF` | `16777216` | vsock socket buffer in bytes; `0` keeps the kernel default (reproduces a slow-upload stall) |
//! | `PUDDLE_AGENT_WINDOW` | `16777216` | yamux receive window per stream (download direction), for experiments |
//! | `PUDDLE_AGENT_OOM` | `1` | `0` turns the OOM watch off |
//! | `PUDDLE_AGENT_VMSTAT` | `/proc/vmstat` | where the `oom_kill` counter is read |
//! | `PUDDLE_AGENT_KMSG` | `/dev/kmsg` | where `Killed process` lines are read |
//! | `PUDDLE_AGENT_OOM_POLL_MS` | `250` | how often the counter is read |
//! | `PUDDLE_AGENT_OOM_GRACE_MS` | `1000` | how long a counter increase waits for its kernel log line before it is reported unnamed |
//! | `PUDDLE_AGENT_ENTRYPOINT_EXIT` | `/run/puddle/entrypoint.exit` | where the boot hook's wrapper writes the image entrypoint's exit, read once a second and reported to the host; `off` turns the watch off |
//! | `PUDDLE_AGENT_PORTS` | `1` | `0` turns the listening-ports watch off (the workspace's Ports tab then lists nothing; forwards to a port still work) |
//! | `PUDDLE_AGENT_PORTS_DIR` | `/proc/net` | where the `tcp` and `tcp6` socket tables are read |
//! | `PUDDLE_AGENT_PORTS_POLL_MS` | `1000` | how often they are read |
//! | `PUDDLE_AGENT_DNS` | `off` | the stub DNS server for tools that ignore the proxy settings: `on` (`198.18.0.1:53`), `off`, or `<ipv4>:<port>`. Binds exactly that address, retrying until an interface has it |
//! | `PUDDLE_AGENT_DNS_TABLE` | `/run/puddle/dns-table` | where the stub keeps its name ⇄ address table across restarts; `off` keeps none |
//! | `PUDDLE_AGENT_EDITOR_DIR` (+ `_PORT`, `_DATA`, `_FOLDER`, `_GRACE`) | unset | the browser editor's server: the release folder switches it on; the rest (`_KIND` for Microsoft's server, `_TELEMETRY`) are in `puddle_agent_proto::editor` |
//! | `PUDDLE_AGENT_EDITOR_PASSWORD_FILE` | `/run/puddle/editor-password` | where the server's password is written |
//! | `PUDDLE_AGENT_CAPTURE` | `off` | transparent capture for tools that ignore the proxy settings: `on` installs the redirect rules, keeps them in place and listens for the redirected connections; it needs the stub DNS (`PUDDLE_AGENT_DNS` set to an address, or unset, which then means `on`), and `PUDDLE_AGENT_DNS=off` with it is an error. Linux guest only |
//! | `PUDDLE_AGENT_CAPTURE_PORT` | `3129` | the port redirected connections arrive on (all addresses: containers' connections arrive on the bridge address) |
//! | `PUDDLE_AGENT_CAPTURE_IFACE` | `dummy0` | the dummy interface that carries the stub's address; made when missing |
//! | `PUDDLE_AGENT_CAPTURE_POLL_MS` | `500` | how often the rules are compared with the kernel's table |
//! | `PUDDLE_AGENT_CAPTURE_LOG_GROUP` | `1291` | the netfilter log group refused UDP is read from; `0` leaves the logging out |
//! | `PUDDLE_AGENT_SSH_AGENT` | `off` | the guest's SSH agent socket (`/run/puddle/ssh-agent.sock` when the host turns it on): each client is carried to the host's filtered agent; `off` makes none |
//! | `PUDDLE_AGENT_SSH_AGENT_USER` | `root` | the session user, who owns the socket (mode 0600) |
//! | `PUDDLE_AGENT_LOG` | `info` | `error`, `warn`, `info`, `debug` or `trace` (to stderr) |

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use puddle_agent_proto::editor::{self, EditorServer};
use puddle_agent_proto::yamux::STREAM_WINDOW;

use crate::editor::EditorConfig;

/// The vsock buffer the agent asks for: above msb's 8 MiB, so the guest's send window is the
/// host's and msb's 4 MiB credit updates arrive.
pub const DEFAULT_VSOCK_BUFFER: u64 = 16 * 1024 * 1024;

/// Most yamux sessions the agent keeps.
const MAX_MUX: usize = 64;

/// Longest value echoed back in a [`ConfigError`].
const MAX_ECHO: usize = 64;

/// Where the host side of the agent's route is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A vsock address (the real route; Linux guest only).
    Vsock {
        /// Context id; 2 is the host.
        cid: u32,
        /// Port of the route.
        port: u32,
    },
    /// A Unix socket, or on Windows a named pipe (tests and local runs without a VM).
    Unix(PathBuf),
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("vsock://") {
            let (cid, port) = rest
                .split_once(':')
                .ok_or("expected vsock://<cid>:<port>")?;
            let cid = cid.parse().map_err(|_| "bad vsock cid")?;
            let port = port.parse().map_err(|_| "bad vsock port")?;
            return Ok(Self::Vsock { cid, port });
        }
        if let Some(path) = s.strip_prefix("unix://")
            && !path.is_empty()
        {
            return Ok(Self::Unix(PathBuf::from(path)));
        }
        Err("expected vsock://<cid>:<port> or unix:///<path>".to_owned())
    }
}

/// Where and how often the OOM watch looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OomSources {
    /// File with the `oom_kill` counter (`/proc/vmstat`).
    pub vmstat: PathBuf,
    /// Kernel log device (`/dev/kmsg`).
    pub kmsg: PathBuf,
    /// Interval between counter reads.
    pub poll: Duration,
    /// How long a counter increase waits for its log line.
    pub grace: Duration,
}

impl Default for OomSources {
    fn default() -> Self {
        Self {
            vmstat: PathBuf::from("/proc/vmstat"),
            kmsg: PathBuf::from("/dev/kmsg"),
            poll: Duration::from_millis(250),
            grace: Duration::from_secs(1),
        }
    }
}

/// Where the image entrypoint's exit is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrypointWatch {
    /// The file the boot hook's wrapper writes: `<boot id> <status> <end of output>`.
    pub exit_file: PathBuf,
    /// The kernel's id of this boot, which an exit must carry to count.
    pub boot_id: PathBuf,
    /// How often the file is read.
    pub poll: Duration,
}

impl Default for EntrypointWatch {
    fn default() -> Self {
        Self {
            exit_file: PathBuf::from("/run/puddle/entrypoint.exit"),
            boot_id: PathBuf::from("/proc/sys/kernel/random/boot_id"),
            poll: Duration::from_secs(1),
        }
    }
}

/// Where and how often the listening-ports watch looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortWatch {
    /// The folder with the kernel's `tcp` and `tcp6` socket tables (`/proc/net`).
    pub dir: PathBuf,
    /// Interval between reads.
    pub poll: Duration,
}

impl Default for PortWatch {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("/proc/net"),
            poll: Duration::from_secs(1),
        }
    }
}

/// The Docker bridge listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeConfig {
    /// The bridge's address and the proxy port containers use. Bound only while a bridge
    /// interface owns the address.
    pub addr: SocketAddrV4,
    /// How often to look for the bridge.
    pub poll: Duration,
}

impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            addr: SocketAddrV4::new(Ipv4Addr::new(172, 17, 0, 1), 3128),
            poll: Duration::from_millis(200),
        }
    }
}

/// The stub DNS server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DnsConfig {
    /// The one address the stub binds (UDP and TCP). Never a wildcard: the stub must not answer
    /// on an interface the guest's own services use.
    pub listen: SocketAddrV4,
    /// The file the stand-in table is kept in, `None` for none.
    pub table: Option<PathBuf>,
}

impl Default for DnsConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddrV4::new(Ipv4Addr::new(198, 18, 0, 1), 53),
            table: Some(PathBuf::from("/run/puddle/dns-table")),
        }
    }
}

/// Transparent capture: the redirect rules and the listener they send connections to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureConfig {
    /// The listener's port.
    pub port: u16,
    /// The dummy interface that carries the stub's address.
    pub interface: String,
    /// How often the rules are compared with the kernel's table.
    pub poll: Duration,
    /// The netfilter log group refused UDP is read from, `None` for no logging.
    pub log_group: Option<u16>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            port: 3129,
            interface: "dummy0".to_owned(),
            poll: Duration::from_millis(500),
            log_group: Some(1291),
        }
    }
}

/// The guest's SSH agent socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAgentConfig {
    /// Where the socket listens.
    pub socket: PathBuf,
    /// The session user, who owns the socket.
    pub user: String,
    /// How long to keep looking for the session user before the socket stays root's.
    pub owner_wait: Duration,
}

/// Everything the agent needs to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Proxy listener inside the guest.
    pub listen: SocketAddr,
    /// The Docker bridge listener, `None` when off.
    pub bridge: Option<BridgeConfig>,
    /// The host.
    pub target: Target,
    /// yamux sessions to keep.
    pub mux: usize,
    /// vsock socket buffer in bytes (`0` = kernel default).
    pub vsock_buffer: u64,
    /// yamux receive window per stream.
    pub window: u32,
    /// OOM watch sources, `None` when the watch is off.
    pub oom: Option<OomSources>,
    /// The listening-ports watch, `None` when it is off.
    pub ports: Option<PortWatch>,
    /// The stub DNS server, `None` when it is off.
    pub dns: Option<DnsConfig>,
    /// The watch on the image entrypoint's exit, `None` when it is off.
    pub entrypoint: Option<EntrypointWatch>,
    /// The browser editor's server, `None` when the host asks for none.
    pub editor: Option<EditorConfig>,
    /// Transparent capture, `None` when it is off. Needs [`Config::dns`].
    pub capture: Option<CaptureConfig>,
    /// The guest's SSH agent socket, `None` when it is off.
    pub ssh_agent: Option<SshAgentConfig>,
    /// Log level.
    pub log: tracing::Level,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 3128)),
            bridge: Some(BridgeConfig::default()),
            target: Target::Vsock { cid: 2, port: 5000 },
            mux: 4,
            vsock_buffer: DEFAULT_VSOCK_BUFFER,
            window: STREAM_WINDOW,
            oom: Some(OomSources::default()),
            ports: Some(PortWatch::default()),
            dns: None,
            entrypoint: Some(EntrypointWatch::default()),
            editor: None,
            capture: None,
            ssh_agent: None,
            log: tracing::Level::INFO,
        }
    }
}

/// A setting with a value the agent can't use.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{var}={value:?}: {reason}")]
pub struct ConfigError {
    /// The variable.
    pub var: &'static str,
    /// Its value, cut to 64 characters.
    pub value: String,
    /// What's wrong.
    pub reason: String,
}

impl Config {
    /// The ports the agent itself serves in the guest (the proxy, the capture listener, the
    /// editor server). The ports watch leaves them out of its reports and the host cannot forward
    /// to them: they are Puddle's plumbing, not something the user runs.
    #[must_use]
    pub fn reserved_ports(&self) -> Vec<u16> {
        let mut ports = vec![self.listen.port()];
        if let Some(bridge) = &self.bridge {
            ports.push(bridge.addr.port());
        }
        if let Some(capture) = &self.capture {
            ports.push(capture.port);
        }
        if let Some(editor) = &self.editor {
            ports.push(editor.server.port);
        }
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Reads the settings through `get` (the process environment in `main`). Unset variables
    /// take their defaults; a set but invalid one is an error, never silently replaced.
    ///
    /// # Errors
    ///
    /// The first variable with a value that doesn't parse or is out of range.
    #[expect(
        clippy::too_many_lines,
        reason = "one flat list of variables, each with its own check"
    )]
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let lookup = |var: &'static str| get(var).map(|v| (v, var));
        if let Some((v, var)) = lookup("PUDDLE_AGENT_LISTEN") {
            config.listen = v
                .parse()
                .map_err(|_| bad(var, &v, "expected <ip>:<port>"))?;
        }
        let mut bridge = BridgeConfig::default();
        if let Some((v, var)) = lookup("PUDDLE_AGENT_BRIDGE_POLL_MS") {
            bridge.poll = millis(var, &v)?;
        }
        config.bridge = match lookup("PUDDLE_AGENT_BRIDGE") {
            None => Some(bridge),
            Some((v, _)) if v == "off" => None,
            Some((v, var)) => {
                bridge.addr = bridge_addr(var, &v)?;
                Some(bridge)
            }
        };
        if let Some((v, var)) = lookup("PUDDLE_AGENT_TARGET") {
            config.target = v.parse().map_err(|reason: String| bad(var, &v, &reason))?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_MUX") {
            config.mux = v
                .parse()
                .ok()
                .filter(|n| (1..=MAX_MUX).contains(n))
                .ok_or_else(|| bad(var, &v, "expected 1 to 64"))?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_VSOCK_BUF") {
            config.vsock_buffer = v.parse().map_err(|_| bad(var, &v, "expected bytes"))?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_WINDOW") {
            config.window = v.parse().map_err(|_| bad(var, &v, "expected bytes"))?;
        }
        let mut oom = OomSources::default();
        if let Some((v, var)) = lookup("PUDDLE_AGENT_VMSTAT") {
            oom.vmstat = path(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_KMSG") {
            oom.kmsg = path(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_OOM_POLL_MS") {
            oom.poll = millis(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_OOM_GRACE_MS") {
            oom.grace = millis(var, &v)?;
        }
        config.oom = match lookup("PUDDLE_AGENT_OOM") {
            None => Some(oom),
            Some((v, _)) if v == "1" => Some(oom),
            Some((v, _)) if v == "0" => None,
            Some((v, var)) => return Err(bad(var, &v, "expected 0 or 1")),
        };
        config.entrypoint = match lookup("PUDDLE_AGENT_ENTRYPOINT_EXIT") {
            None => Some(EntrypointWatch::default()),
            Some((v, _)) if v == "off" => None,
            Some((v, var)) => Some(EntrypointWatch {
                exit_file: path(var, &v)?,
                ..EntrypointWatch::default()
            }),
        };
        let mut ports = PortWatch::default();
        if let Some((v, var)) = lookup("PUDDLE_AGENT_PORTS_DIR") {
            ports.dir = path(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_PORTS_POLL_MS") {
            ports.poll = millis(var, &v)?;
        }
        config.ports = match lookup("PUDDLE_AGENT_PORTS") {
            None => Some(ports),
            Some((v, _)) if v == "1" => Some(ports),
            Some((v, _)) if v == "0" => None,
            Some((v, var)) => return Err(bad(var, &v, "expected 0 or 1")),
        };
        let mut dns = DnsConfig::default();
        if let Some((v, var)) = lookup("PUDDLE_AGENT_DNS_TABLE") {
            dns.table = if v == "off" {
                None
            } else {
                Some(path(var, &v)?)
            };
        }
        let mut capture = CaptureConfig::default();
        if let Some((v, var)) = lookup("PUDDLE_AGENT_CAPTURE_PORT") {
            capture.port = v
                .parse()
                .ok()
                .filter(|p| *p != 0)
                .ok_or_else(|| bad(var, &v, "expected a port, 1 to 65535"))?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_CAPTURE_IFACE") {
            capture.interface = interface_name(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_CAPTURE_POLL_MS") {
            capture.poll = millis(var, &v)?;
        }
        if let Some((v, var)) = lookup("PUDDLE_AGENT_CAPTURE_LOG_GROUP") {
            let group: u16 = v
                .parse()
                .map_err(|_| bad(var, &v, "expected a log group, 0 to 65535"))?;
            capture.log_group = (group != 0).then_some(group);
        }
        config.ssh_agent = match lookup("PUDDLE_AGENT_SSH_AGENT") {
            None => None,
            Some((v, _)) if v == "off" => None,
            Some((v, var)) => Some(SshAgentConfig {
                socket: path(var, &v)?,
                user: lookup("PUDDLE_AGENT_SSH_AGENT_USER")
                    .map_or_else(|| "root".to_owned(), |(user, _)| user),
                owner_wait: Duration::from_secs(60),
            }),
        };
        let capture_on = match lookup("PUDDLE_AGENT_CAPTURE") {
            None => false,
            Some((v, _)) if v == "off" => false,
            Some((v, _)) if v == "on" => true,
            Some((v, var)) => return Err(bad(var, &v, "expected on or off")),
        };
        config.dns = match lookup("PUDDLE_AGENT_DNS") {
            None => capture_on.then_some(dns),
            Some((v, var)) if v == "off" => {
                if capture_on {
                    return Err(bad(
                        var,
                        &v,
                        "capture needs the stub DNS: its redirect sends DNS there",
                    ));
                }
                None
            }
            Some((v, _)) if v == "on" => Some(dns),
            Some((v, var)) => {
                dns.listen = dns_addr(var, &v)?;
                Some(dns)
            }
        };
        config.editor = EditorServer::from_lookup(&get)
            .map_err(|e| bad(e.var, &get(e.var).unwrap_or_default(), &e.reason))?
            .map(|server| {
                let password_file = get("PUDDLE_AGENT_EDITOR_PASSWORD_FILE")
                    .filter(|p| !p.is_empty())
                    .map_or_else(|| PathBuf::from(editor::PASSWORD_FILE), PathBuf::from);
                EditorConfig {
                    server,
                    password_file,
                    boot_id: PathBuf::from(crate::editor::BOOT_ID_FILE),
                    root: PathBuf::from("/"),
                }
            });
        config.capture = capture_on.then_some(capture);
        if let Some((v, var)) = lookup("PUDDLE_AGENT_LOG") {
            config.log = v
                .parse()
                .map_err(|_| bad(var, &v, "expected error, warn, info, debug or trace"))?;
        }
        Ok(config)
    }
}

fn bad(var: &'static str, value: &str, reason: &str) -> ConfigError {
    ConfigError {
        var,
        value: value.chars().take(MAX_ECHO).collect(),
        reason: reason.to_owned(),
    }
}

/// A bridge address must be a specific IPv4 address a container can route to: never a wildcard
/// (that would listen on every interface), loopback or multicast.
fn bridge_addr(var: &'static str, value: &str) -> Result<SocketAddrV4, ConfigError> {
    let addr: SocketAddrV4 = value
        .parse()
        .map_err(|_| bad(var, value, "expected <ipv4>:<port> or off"))?;
    let ip = addr.ip();
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.is_broadcast() {
        return Err(bad(
            var,
            value,
            "expected a specific, non-loopback bridge address",
        ));
    }
    if addr.port() == 0 {
        return Err(bad(var, value, "expected a fixed port"));
    }
    Ok(addr)
}

/// The stub binds one specific IPv4 address (a wildcard would answer on every interface).
fn dns_addr(var: &'static str, value: &str) -> Result<SocketAddrV4, ConfigError> {
    let addr: SocketAddrV4 = value
        .parse()
        .map_err(|_| bad(var, value, "expected <ipv4>:<port>, on or off"))?;
    if addr.ip().is_unspecified() || addr.ip().is_multicast() || addr.ip().is_broadcast() {
        return Err(bad(
            var,
            value,
            "expected a specific address, not a wildcard",
        ));
    }
    Ok(addr)
}

/// An interface name the kernel accepts: 1 to 15 bytes of letters, digits, `-`, `_` and `.`.
fn interface_name(var: &'static str, value: &str) -> Result<String, ConfigError> {
    let ok = !value.is_empty()
        && value.len() <= 15
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(value.to_owned())
    } else {
        Err(bad(
            var,
            value,
            "expected an interface name of at most 15 letters, digits, - _ .",
        ))
    }
}

fn path(var: &'static str, value: &str) -> Result<PathBuf, ConfigError> {
    if value.is_empty() {
        return Err(bad(var, value, "expected a path"));
    }
    Ok(PathBuf::from(value))
}

fn millis(var: &'static str, value: &str) -> Result<Duration, ConfigError> {
    value
        .parse()
        .ok()
        .filter(|&ms| ms > 0 && ms <= 60_000)
        .map(Duration::from_millis)
        .ok_or_else(|| bad(var, value, "expected 1 to 60000 ms"))
}

#[cfg(test)]
mod tests;
