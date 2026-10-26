// SPDX-License-Identifier: GPL-3.0-or-later
//! Guest agent (`puddle-agent`): a static binary inside each sandbox (ADR 0005) that
//!
//! - listens for proxy clients (`HTTP(S)_PROXY`) and
//!   carries each connection to Puddle's proxy as one yamux stream over vsock ([`serve`],
//!   [`upstream`]);
//! - also listens on the Docker bridge address (`172.17.0.1:3128`) while a bridge owns it, so
//!   containers inside the sandbox reach the same proxy ([`bridge`]);
//! - watches the guest kernel's OOM killer and reports each kill on the control stream
//!   ([`oom`], [`control`]), which the host turns into
//!   [`puddle_types::Event::OomKill`];
//! - reports an exit of the image's entrypoint ([`entrypoint`]) and what the stub DNS turns away,
//!   which the host turns into problems on the workspace;
//! - reports the TCP ports the guest listens on ([`ports`]) and serves the host's port forwards:
//!   the host opens a stream to the agent, which connects to the port on the guest's loopback
//!   ([`forward`]);
//! - answers DNS for tools that ignore the proxy settings, when switched on: a stub on one
//!   address hands out stand-in addresses and asks the host about each name ([`capture`]);
//! - with capture on (Linux), redirects those tools' connections to a second listener that sends
//!   them to the proxy by name, installs and guards the rules that do it, and fails other UDP at
//!   once ([`capture`]).
//!
//! - runs the browser editor's server (code-server, read-only from the host) and keeps it running
//!   ([`editor`]), when the host asks.
//!
//! It also applies merged guest files for the boot hook ([`merge_file`]) and is the `ProxyCommand`
//! of the guest's `ssh` ([`connect`]), which today only refuses: SSH is not supported yet.
//!
//! The wire protocol is in `puddle-agent-proto`. Settings come from environment variables
//! ([`config`]). The binary's `main` only parses the command line ([`cli`]) and starts an
//! [`Agent`].

pub mod bridge;
pub mod capture;
pub mod cli;
pub mod config;
pub mod connect;
pub mod control;
pub mod editor;
pub mod entrypoint;
pub mod exthost;
pub mod forward;
#[cfg(target_os = "linux")]
mod ifaddrs;
pub mod merge_file;
pub mod oom;
pub mod ports;
pub mod serve;
pub mod upstream;
#[cfg(target_os = "linux")]
mod vsock;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use puddle_agent_proto::AgentMessage;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

pub use config::Config;

/// Reports waiting for the control stream.
const REPORT_QUEUE: usize = 64;

/// A running agent. Its tasks stop when it is dropped.
#[derive(Debug)]
pub struct Agent {
    local_addr: SocketAddr,
    bridge: watch::Receiver<bridge::BridgeState>,
    stand_ins: Option<capture::table::StandIns>,
    dns: Option<(SocketAddr, SocketAddr)>,
    capture: Option<SocketAddr>,
    tasks: JoinSet<()>,
    /// Keeps the control stream open when the OOM watch is off.
    _reports: Option<mpsc::Sender<AgentMessage>>,
}

impl Agent {
    /// Binds the listener and starts the proxy relay, the control stream and (if on) the OOM
    /// watch. The host is dialled on first use.
    ///
    /// # Errors
    ///
    /// Binding the listener failing.
    pub async fn start(config: Config) -> io::Result<Self> {
        Self::start_with_probe(config, bridge::system_probe()).await
    }

    /// [`Agent::start`] with a chosen way of finding the Docker bridge (tests).
    ///
    /// # Errors
    ///
    /// Binding the listener failing.
    pub async fn start_with_probe(
        config: Config,
        probe: Arc<dyn bridge::Probe>,
    ) -> io::Result<Self> {
        let listener = serve::listen(config.listen)?;
        let local_addr = listener.local_addr()?;
        // The listener may have taken any port (`:0` in tests): the agent's own ports are the real ones.
        let bound = Config {
            listen: local_addr,
            ..config.clone()
        };
        let upstream = Arc::new(upstream::Upstream::new(&bound));
        let mut tasks = JoinSet::new();
        tasks.spawn(serve::serve(listener, Arc::clone(&upstream)));
        let (bridge_tx, bridge) = watch::channel(bridge::BridgeState::default());
        match config.bridge {
            // A wildcard listener on the bridge's port already covers the bridge address (and
            // would make the bind fail), so there is nothing to watch for.
            Some(b) if covers(config.listen, b.addr.into()) => {
                tracing::info!(listen = %config.listen, "the proxy listener already covers the docker bridge");
            }
            Some(b) => {
                tasks.spawn(bridge::watch(b, probe, Arc::clone(&upstream), bridge_tx));
            }
            None => {}
        }
        let (tx, rx) = mpsc::channel(REPORT_QUEUE);
        let (stand_ins, dns) = match &config.dns {
            Some(dns_config) => {
                let (table, bound) = start_dns(dns_config, &upstream, &tx, &mut tasks).await;
                (Some(table), bound)
            }
            None => (None, None),
        };
        if let Some(editor) = config.editor.clone() {
            tasks.spawn(editor::run(editor));
        }
        if let Some(watch) = config.entrypoint.clone() {
            tasks.spawn(entrypoint::watch(watch, tx.clone()));
        }
        let capture =
            start_capture(&config, stand_ins.as_ref(), &upstream, &tx, &mut tasks).await?;
        tasks.spawn(control::run(upstream, rx));
        if let Some(watch) = config.ports.clone() {
            tasks.spawn(ports::watch(watch, bound.reserved_ports(), tx.clone()));
        }
        let reports = match config.oom {
            Some(sources) => {
                tasks.spawn(oom::watch(sources, tx));
                None
            }
            None => Some(tx),
        };
        tracing::info!(listen = %local_addr, target = ?config.target, sessions = config.mux, "puddle-agent started");
        Ok(Self {
            local_addr,
            bridge,
            stand_ins,
            dns,
            capture,
            tasks,
            _reports: reports,
        })
    }

    /// The address the proxy listener is bound to.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Where the Docker bridge listener is bound right now, `None` while there is no bridge
    /// (or the listener is off).
    #[must_use]
    pub fn bridge_addr(&self) -> Option<SocketAddr> {
        self.bridge.borrow().addr
    }

    /// The stand-in table, `None` while the stub DNS is off. The transparent listener finds the
    /// name behind a redirected connection's original address in it.
    #[must_use]
    pub fn stand_ins(&self) -> Option<capture::table::StandIns> {
        self.stand_ins.clone()
    }

    /// The UDP and TCP addresses the stub DNS bound at start, `None` when it is off or its address
    /// didn't exist yet (it keeps trying).
    #[must_use]
    pub fn dns_addrs(&self) -> Option<(SocketAddr, SocketAddr)> {
        self.dns
    }

    /// The address the transparent listener is bound to, `None` when capture is off.
    #[must_use]
    pub fn capture_addr(&self) -> Option<SocketAddr> {
        self.capture
    }

    /// Follows the Docker bridge listener: every bind and every close is a new state.
    #[must_use]
    pub fn bridge_state(&self) -> watch::Receiver<bridge::BridgeState> {
        self.bridge.clone()
    }

    /// Runs until the listener stops (it doesn't on its own).
    pub async fn run(mut self) {
        while self.tasks.join_next().await.is_some() {}
    }
}

/// Starts the stub DNS. If its address isn't there yet (the interface is made after the agent
/// starts), a task keeps trying; the table is usable at once.
async fn start_dns(
    config: &config::DnsConfig,
    upstream: &Arc<upstream::Upstream>,
    reports: &mpsc::Sender<AgentMessage>,
    tasks: &mut JoinSet<()>,
) -> (capture::table::StandIns, Option<(SocketAddr, SocketAddr)>) {
    let table = match &config.table {
        Some(path) => capture::table::StandIns::open(path),
        None => capture::table::StandIns::in_memory(),
    };
    let limits = capture::dns::Limits::default();
    let stub = capture::dns::Stub::reporting(
        capture::dns::HostAsk::new(Arc::clone(upstream), limits.ask_timeout),
        table.clone(),
        limits,
        Some(reports.clone()),
    );
    let listen = SocketAddr::V4(config.listen);
    match capture::dns::DnsServer::bind(stub.clone(), listen).await {
        Ok(server) => {
            let addrs = (server.udp_addr(), server.tcp_addr());
            tracing::info!(%listen, "stub DNS listening");
            tasks.spawn(server.run());
            (table, Some(addrs))
        }
        Err(err) => {
            tracing::warn!(%listen, error = %err, "stub DNS not bound yet; trying again until it is");
            tasks.spawn(capture::dns::bind_when_ready(stub, listen));
            (table, None)
        }
    }
}

/// Starts transparent capture when it is on: the listener the redirect sends connections to, the
/// guard that installs and keeps the rules, and the reader of refused UDP.
#[cfg(target_os = "linux")]
async fn start_capture(
    config: &Config,
    stand_ins: Option<&capture::table::StandIns>,
    upstream: &Arc<upstream::Upstream>,
    reports: &mpsc::Sender<AgentMessage>,
    tasks: &mut JoinSet<()>,
) -> io::Result<Option<SocketAddr>> {
    let (Some(cap), Some(dns), Some(table)) = (&config.capture, &config.dns, stand_ins) else {
        return Ok(None);
    };
    // All addresses: a redirected connection from a container arrives on the bridge address. A
    // process that dials the port itself has no original destination and is closed at once.
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cap.port)).await?;
    let addr = listener.local_addr()?;
    tasks.spawn(capture::listen::serve(
        listener,
        table.clone(),
        Arc::clone(upstream),
    ));
    if let Some(group) = cap.log_group {
        tasks.spawn(capture::udp::watch(group, reports.clone()));
    }
    let guard = capture::guard::Guard {
        stub: dns.listen,
        listen_port: addr.port(),
        interface: cap.interface.clone(),
        poll: cap.poll,
        log_group: cap.log_group,
    };
    tasks.spawn(capture::guard::keep(guard, reports.clone()));
    tracing::info!(listen = %addr, "transparent capture on");
    Ok(Some(addr))
}

/// Capture needs the Linux guest's netfilter; elsewhere it is ignored with a warning.
#[cfg(not(target_os = "linux"))]
#[expect(clippy::unused_async, reason = "same signature as the Linux version")]
async fn start_capture(
    config: &Config,
    _stand_ins: Option<&capture::table::StandIns>,
    _upstream: &Arc<upstream::Upstream>,
    _reports: &mpsc::Sender<AgentMessage>,
    _tasks: &mut JoinSet<()>,
) -> io::Result<Option<SocketAddr>> {
    if config.capture.is_some() {
        tracing::warn!("transparent capture needs a Linux guest; ignored");
    }
    Ok(None)
}

/// Whether a listener on `listen` accepts connections for `addr`.
fn covers(listen: SocketAddr, addr: SocketAddr) -> bool {
    listen.port() == addr.port() && (listen.ip().is_unspecified() || listen.ip() == addr.ip())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_wildcard_or_equal_listener_covers_the_bridge_address() {
        let bridge: SocketAddr = "172.17.0.1:3128".parse().unwrap();
        assert!(covers("0.0.0.0:3128".parse().unwrap(), bridge));
        assert!(covers("172.17.0.1:3128".parse().unwrap(), bridge));
        assert!(!covers("0.0.0.0:3129".parse().unwrap(), bridge));
        assert!(!covers("127.0.0.1:3128".parse().unwrap(), bridge));
    }
}
