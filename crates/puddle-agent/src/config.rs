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
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn from(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn no_variables_give_the_defaults() {
        let c = from(&[]).unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.listen.to_string(), "127.0.0.1:3128");
        assert_eq!(c.target, Target::Vsock { cid: 2, port: 5000 });
        assert_eq!(c.vsock_buffer, 16 << 20);
        assert_eq!(c.oom, Some(OomSources::default()));
        assert_eq!(c.ports, Some(PortWatch::default()));
    }

    #[test]
    fn the_ports_watch_has_its_own_switches_and_the_agents_own_ports_are_reserved() {
        let c = from(&[
            ("PUDDLE_AGENT_PORTS_DIR", "/x/net"),
            ("PUDDLE_AGENT_PORTS_POLL_MS", "50"),
        ])
        .unwrap();
        assert_eq!(
            c.ports,
            Some(PortWatch {
                dir: PathBuf::from("/x/net"),
                poll: Duration::from_millis(50)
            })
        );
        assert_eq!(from(&[("PUDDLE_AGENT_PORTS", "0")]).unwrap().ports, None);
        for (var, value) in [
            ("PUDDLE_AGENT_PORTS", "yes"),
            ("PUDDLE_AGENT_PORTS_DIR", ""),
            ("PUDDLE_AGENT_PORTS_POLL_MS", "0"),
        ] {
            assert_eq!(from(&[(var, value)]).unwrap_err().var, var);
        }
        // The proxy and the Docker bridge listener share a port by default; the others follow
        // what is switched on.
        assert_eq!(from(&[]).unwrap().reserved_ports(), [3128]);
        let c = from(&[
            ("PUDDLE_AGENT_BRIDGE", "172.17.0.1:3130"),
            ("PUDDLE_AGENT_CAPTURE", "on"),
            ("PUDDLE_AGENT_CAPTURE_PORT", "3129"),
            ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
            ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
            ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
            ("PUDDLE_AGENT_EDITOR_PORT", "41337"),
            ("PUDDLE_AGENT_EDITOR_GRACE", "300"),
        ])
        .unwrap();
        assert_eq!(c.reserved_ports(), [3128, 3129, 3130, 41_337]);
    }

    #[test]
    fn the_editor_server_is_off_until_the_host_names_its_folder() {
        assert_eq!(
            from(&[("PUDDLE_AGENT_EDITOR_PORT", "4000")])
                .unwrap()
                .editor,
            None
        );
        let c = from(&[
            ("PUDDLE_AGENT_EDITOR_DIR", "/puddle/code-server"),
            (
                "PUDDLE_AGENT_EDITOR_DATA",
                "/workspaces/a/.puddle/code-server",
            ),
            ("PUDDLE_AGENT_EDITOR_FOLDER", "/workspaces/a"),
            ("PUDDLE_AGENT_EDITOR_GRACE", "60"),
        ])
        .unwrap();
        let editor = c.editor.unwrap();
        assert_eq!(editor.server.grace_secs, 60);
        assert_eq!(editor.server.port, 41_337);
        assert_eq!(
            editor.password_file,
            PathBuf::from("/run/puddle/editor-password")
        );
        let c = from(&[
            ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
            ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
            ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
            ("PUDDLE_AGENT_EDITOR_PASSWORD_FILE", "/x/pw"),
        ])
        .unwrap();
        assert_eq!(c.editor.unwrap().password_file, PathBuf::from("/x/pw"));
    }

    #[test]
    fn an_unusable_editor_variable_is_named_and_never_replaced() {
        let err = from(&[
            ("PUDDLE_AGENT_EDITOR_DIR", "/e"),
            ("PUDDLE_AGENT_EDITOR_DATA", "/d"),
            ("PUDDLE_AGENT_EDITOR_FOLDER", "/f"),
            ("PUDDLE_AGENT_EDITOR_GRACE", "5"),
        ])
        .unwrap_err();
        assert_eq!(err.var, "PUDDLE_AGENT_EDITOR_GRACE");
        assert_eq!(err.value, "5");
        let err = from(&[("PUDDLE_AGENT_EDITOR_DIR", "/e")]).unwrap_err();
        assert_eq!(err.var, "PUDDLE_AGENT_EDITOR_DATA");
    }

    #[test]
    fn capture_is_off_unless_asked_for_and_then_brings_the_stub_with_it() {
        assert_eq!(from(&[]).unwrap().capture, None);
        assert_eq!(
            from(&[("PUDDLE_AGENT_CAPTURE", "off")]).unwrap().capture,
            None
        );
        let c = from(&[("PUDDLE_AGENT_CAPTURE", "on")]).unwrap();
        assert_eq!(c.capture, Some(CaptureConfig::default()));
        assert_eq!(
            c.dns,
            Some(DnsConfig::default()),
            "the redirect sends DNS to the stub"
        );
        let c = from(&[
            ("PUDDLE_AGENT_CAPTURE", "on"),
            ("PUDDLE_AGENT_DNS", "198.18.0.9:5353"),
            ("PUDDLE_AGENT_CAPTURE_PORT", "4000"),
            ("PUDDLE_AGENT_CAPTURE_IFACE", "cap0"),
            ("PUDDLE_AGENT_CAPTURE_POLL_MS", "100"),
            ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "7"),
        ])
        .unwrap();
        let capture = c.capture.unwrap();
        assert_eq!(
            (
                capture.port,
                capture.interface.as_str(),
                capture.poll,
                capture.log_group
            ),
            (4000, "cap0", Duration::from_millis(100), Some(7))
        );
        assert_eq!(c.dns.unwrap().listen.to_string(), "198.18.0.9:5353");
        let c = from(&[
            ("PUDDLE_AGENT_CAPTURE", "on"),
            ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "0"),
        ])
        .unwrap();
        assert_eq!(c.capture.unwrap().log_group, None);
    }

    #[test]
    fn capture_without_its_stub_or_with_odd_values_is_an_error() {
        let err = from(&[("PUDDLE_AGENT_CAPTURE", "on"), ("PUDDLE_AGENT_DNS", "off")]).unwrap_err();
        assert_eq!(err.var, "PUDDLE_AGENT_DNS");
        for (var, value) in [
            ("PUDDLE_AGENT_CAPTURE", "yes"),
            ("PUDDLE_AGENT_CAPTURE_PORT", "0"),
            ("PUDDLE_AGENT_CAPTURE_PORT", "70000"),
            ("PUDDLE_AGENT_CAPTURE_IFACE", ""),
            ("PUDDLE_AGENT_CAPTURE_IFACE", "a/b"),
            ("PUDDLE_AGENT_CAPTURE_IFACE", "sixteen-chars-xxx"),
            ("PUDDLE_AGENT_CAPTURE_POLL_MS", "0"),
            ("PUDDLE_AGENT_CAPTURE_LOG_GROUP", "x"),
        ] {
            let err = from(&[("PUDDLE_AGENT_CAPTURE", "on"), (var, value)]).unwrap_err();
            assert_eq!(err.var, var, "{value}");
        }
    }

    #[test]
    fn every_variable_is_read() {
        let c = from(&[
            ("PUDDLE_AGENT_LISTEN", "0.0.0.0:3129"),
            ("PUDDLE_AGENT_TARGET", "unix:///run/p.sock"),
            ("PUDDLE_AGENT_MUX", "2"),
            ("PUDDLE_AGENT_VSOCK_BUF", "0"),
            ("PUDDLE_AGENT_WINDOW", "262144"),
            ("PUDDLE_AGENT_VMSTAT", "/x/vmstat"),
            ("PUDDLE_AGENT_KMSG", "/x/kmsg"),
            ("PUDDLE_AGENT_OOM_POLL_MS", "50"),
            ("PUDDLE_AGENT_OOM_GRACE_MS", "200"),
            ("PUDDLE_AGENT_LOG", "debug"),
        ])
        .unwrap();
        assert_eq!(c.listen.to_string(), "0.0.0.0:3129");
        assert_eq!(c.target, Target::Unix("/run/p.sock".into()));
        assert_eq!((c.mux, c.vsock_buffer, c.window), (2, 0, 262_144));
        let oom = c.oom.unwrap();
        assert_eq!(oom.vmstat, PathBuf::from("/x/vmstat"));
        assert_eq!(oom.kmsg, PathBuf::from("/x/kmsg"));
        assert_eq!(oom.poll, Duration::from_millis(50));
        assert_eq!(oom.grace, Duration::from_millis(200));
        assert_eq!(c.log, tracing::Level::DEBUG);
        assert_eq!(
            from(&[("PUDDLE_AGENT_OOM", "0")]).unwrap().oom,
            None,
            "0 turns the watch off"
        );
        assert!(from(&[("PUDDLE_AGENT_OOM", "1")]).unwrap().oom.is_some());
        assert_eq!(
            from(&[]).unwrap().entrypoint,
            Some(EntrypointWatch::default())
        );
        assert_eq!(
            from(&[("PUDDLE_AGENT_ENTRYPOINT_EXIT", "off")])
                .unwrap()
                .entrypoint,
            None
        );
        assert_eq!(
            from(&[("PUDDLE_AGENT_ENTRYPOINT_EXIT", "/tmp/exit")])
                .unwrap()
                .entrypoint
                .map(|w| w.exit_file),
            Some(PathBuf::from("/tmp/exit"))
        );
    }

    #[test]
    fn invalid_values_are_errors_that_name_the_variable() {
        let cases = [
            ("PUDDLE_AGENT_LISTEN", "3128"),
            ("PUDDLE_AGENT_BRIDGE", "172.17.0.1"),
            ("PUDDLE_AGENT_BRIDGE", "0.0.0.0:3128"),
            ("PUDDLE_AGENT_BRIDGE", "127.0.0.1:3128"),
            ("PUDDLE_AGENT_BRIDGE", "224.0.0.1:3128"),
            ("PUDDLE_AGENT_BRIDGE", "255.255.255.255:3128"),
            ("PUDDLE_AGENT_BRIDGE", "172.17.0.1:0"),
            ("PUDDLE_AGENT_BRIDGE", "[::1]:3128"),
            ("PUDDLE_AGENT_BRIDGE", "on"),
            ("PUDDLE_AGENT_BRIDGE_POLL_MS", "0"),
            ("PUDDLE_AGENT_TARGET", "tcp://1.2.3.4:5"),
            ("PUDDLE_AGENT_TARGET", "vsock://2"),
            ("PUDDLE_AGENT_TARGET", "vsock://x:5000"),
            ("PUDDLE_AGENT_TARGET", "vsock://2:x"),
            ("PUDDLE_AGENT_TARGET", "unix://"),
            ("PUDDLE_AGENT_MUX", "0"),
            ("PUDDLE_AGENT_MUX", "65"),
            ("PUDDLE_AGENT_VSOCK_BUF", "-1"),
            ("PUDDLE_AGENT_WINDOW", "big"),
            ("PUDDLE_AGENT_VMSTAT", ""),
            ("PUDDLE_AGENT_KMSG", ""),
            ("PUDDLE_AGENT_OOM_POLL_MS", "0"),
            ("PUDDLE_AGENT_OOM_GRACE_MS", "60001"),
            ("PUDDLE_AGENT_OOM", "yes"),
            ("PUDDLE_AGENT_DNS", "yes"),
            ("PUDDLE_AGENT_DNS", "0.0.0.0:53"),
            ("PUDDLE_AGENT_DNS", "224.0.0.1:53"),
            ("PUDDLE_AGENT_DNS", "198.18.0.1"),
            ("PUDDLE_AGENT_DNS", "[::1]:53"),
            ("PUDDLE_AGENT_DNS_TABLE", ""),
            ("PUDDLE_AGENT_LOG", "loud"),
        ];
        for (var, value) in cases {
            let err = from(&[(var, value)]).unwrap_err();
            assert_eq!(err.var, var);
            assert!(err.to_string().starts_with(var), "{err}");
        }
    }

    #[test]
    fn the_bridge_listener_is_on_for_the_docker_default_and_can_move_or_stop() {
        let default = from(&[]).unwrap().bridge.unwrap();
        assert_eq!(default.addr.to_string(), "172.17.0.1:3128");
        assert_eq!(default.poll, Duration::from_millis(200));
        let moved = from(&[
            ("PUDDLE_AGENT_BRIDGE", "192.168.200.1:3129"),
            ("PUDDLE_AGENT_BRIDGE_POLL_MS", "50"),
        ])
        .unwrap()
        .bridge
        .unwrap();
        assert_eq!(moved.addr.to_string(), "192.168.200.1:3129");
        assert_eq!(moved.poll, Duration::from_millis(50));
        assert_eq!(
            from(&[("PUDDLE_AGENT_BRIDGE", "off")]).unwrap().bridge,
            None
        );
    }

    #[test]
    fn the_stub_dns_is_off_until_asked_for_and_binds_one_specific_address() {
        assert_eq!(from(&[]).unwrap().dns, None);
        assert_eq!(from(&[("PUDDLE_AGENT_DNS", "off")]).unwrap().dns, None);
        let on = from(&[("PUDDLE_AGENT_DNS", "on")]).unwrap().dns.unwrap();
        assert_eq!(on.listen.to_string(), "198.18.0.1:53");
        assert_eq!(on.table, Some(PathBuf::from("/run/puddle/dns-table")));
        let custom = from(&[
            ("PUDDLE_AGENT_DNS", "127.0.0.1:5353"),
            ("PUDDLE_AGENT_DNS_TABLE", "/x/table"),
        ])
        .unwrap()
        .dns
        .unwrap();
        assert_eq!(custom.listen.to_string(), "127.0.0.1:5353");
        assert_eq!(custom.table, Some(PathBuf::from("/x/table")));
        let no_file = from(&[
            ("PUDDLE_AGENT_DNS", "on"),
            ("PUDDLE_AGENT_DNS_TABLE", "off"),
        ])
        .unwrap()
        .dns
        .unwrap();
        assert_eq!(no_file.table, None);
    }

    #[test]
    fn an_echoed_value_is_cut() {
        let long = "x".repeat(500);
        let err = from(&[("PUDDLE_AGENT_LISTEN", &long)]).unwrap_err();
        assert_eq!(err.value.len(), 64);
    }
}
