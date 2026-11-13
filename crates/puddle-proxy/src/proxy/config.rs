// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy's limits and timeouts.

use std::time::Duration;

use puddle_agent_proto::host::HostConfig;

/// Limits and timeouts. The defaults suit a real guest; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxyConfig {
    /// How long a guest may take to send its whole request head (slow-loris guard). Over it:
    /// `408`.
    pub head_timeout: Duration,
    /// How long resolving an allowed name may take. Over it: `504`.
    pub resolve_timeout: Duration,
    /// How long connecting to one address may take before the next is tried. All failed: `502`.
    pub connect_timeout: Duration,
    /// Proxied connections one workspace may have open at once. Over it: `503`. Far above what
    /// real tools open (pnpm, `BuildKit` and `NuGet` peak in the hundreds).
    pub max_streams_per_workspace: usize,
    /// Agent connections (yamux sessions) one route accepts at once. The agent opens 1 to 64.
    pub max_sessions_per_route: usize,
    /// Per-session limits of the agent protocol.
    pub session: HostConfig,
    /// How long the proxy's own TLS handshake with a bound host may take.
    pub tls_handshake_timeout: Duration,
    /// How long each step of the guest's TLS handshake with the proxy may take: its hello after
    /// the `CONNECT` answer, then the rest of the handshake. The proxy plays the server here, so
    /// the guest gets what common servers give a client by default (nginx's
    /// `ssl_handshake_timeout`, Python's asyncio: 60 s): a client stalled for longer than the
    /// upstream budget, by a busy disk or a paused machine, still connects instead of finding its
    /// connection cut.
    pub guest_handshake_timeout: Duration,
    /// How long a terminated connection, or a guest connection carrying plain HTTP, may sit idle
    /// between two requests before it is closed.
    pub keepalive_timeout: Duration,
    /// How long a bound host may take to answer once the whole request is sent. Over it: `504`.
    pub upstream_head_timeout: Duration,
    /// How long either side of a terminated HTTP/1.1 request may stall in the middle of a body
    /// before the connection is dropped. HTTP/2 streams are kept alive by pings instead.
    pub body_idle_timeout: Duration,
    /// How long one name lookup for the guest's stub DNS may take. Over it, the stub gets
    /// `SERVFAIL` (or, with a company proxy in the route, a stand-in address).
    pub lookup_timeout: Duration,
    /// Name lookups (of allowed names) one workspace may run at once. A lookup over it waits up
    /// to `lookup_timeout` for a free slot; after that the stub gets `SERVFAIL`.
    pub max_lookups_per_workspace: usize,
    /// HTTP/2 streams one workspace may have open at once across its terminated connections.
    /// A stream over it gets a `503`. Each open stream can hold up to a flow-control window of
    /// the guest's data, so this is also the ceiling of the memory hostile guests can pin.
    pub max_h2_streams_per_workspace: usize,
    /// How long an HTTP/2 stream of a terminated connection may move no frame in either direction
    /// before it is reset. A watch or event stream is quiet for long stretches, so this is far
    /// above the body-stall timeout of HTTP/1.1; the connection's ping catches a dead peer sooner.
    pub stream_idle_timeout: Duration,
    /// How long an agreed WebSocket of a terminated connection may move no byte in either
    /// direction before it is closed.
    pub websocket_idle_timeout: Duration,
    /// How long a terminated HTTP/2 connection may take to end after the proxy asked it to shut
    /// down (idle too long, or a stream that would not give up) before it is dropped.
    pub shutdown_grace: Duration,
    /// How many plain-HTTP requests one guest connection may carry. The answer to the last says
    /// `connection: close`, and the guest's client opens a new connection for the next. Each
    /// request is decided and recorded on its own either way; the limit bounds how long one
    /// connection lives. The idle time between two requests is `keepalive_timeout`.
    pub max_requests_per_connection: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            head_timeout: Duration::from_secs(30),
            resolve_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            max_streams_per_workspace: 4096,
            max_sessions_per_route: 128,
            session: HostConfig::default(),
            tls_handshake_timeout: Duration::from_secs(10),
            guest_handshake_timeout: Duration::from_secs(60),
            keepalive_timeout: Duration::from_secs(60),
            upstream_head_timeout: Duration::from_secs(300),
            body_idle_timeout: Duration::from_secs(120),
            lookup_timeout: Duration::from_secs(3),
            max_lookups_per_workspace: 128,
            max_h2_streams_per_workspace: 2048,
            stream_idle_timeout: Duration::from_secs(3600),
            websocket_idle_timeout: Duration::from_secs(3600),
            shutdown_grace: Duration::from_secs(20),
            max_requests_per_connection: 1000,
        }
    }
}

impl ProxyConfig {
    /// The default limits with another name-lookup timeout and per-workspace lookup cap.
    #[must_use]
    pub fn with_lookup_limits(mut self, timeout: Duration, max_per_workspace: usize) -> Self {
        self.lookup_timeout = timeout;
        self.max_lookups_per_workspace = max_per_workspace;
        self
    }

    /// The default limits with another per-workspace HTTP/2 stream budget.
    #[must_use]
    pub fn with_max_h2_streams_per_workspace(mut self, max: usize) -> Self {
        self.max_h2_streams_per_workspace = max;
        self
    }

    /// The default limits with other idle limits for HTTP/2 streams and agreed `WebSocket`s, and
    /// another grace for a connection that was asked to shut down.
    #[must_use]
    pub fn with_idle_limits(
        mut self,
        stream: Duration,
        websocket: Duration,
        shutdown_grace: Duration,
    ) -> Self {
        self.stream_idle_timeout = stream;
        self.websocket_idle_timeout = websocket;
        self.shutdown_grace = shutdown_grace;
        self
    }

    /// The default limits with another head timeout.
    #[must_use]
    pub fn with_head_timeout(mut self, timeout: Duration) -> Self {
        self.head_timeout = timeout;
        self
    }

    /// The default limits with another per-workspace stream cap.
    #[must_use]
    pub fn with_max_streams_per_workspace(mut self, max: usize) -> Self {
        self.max_streams_per_workspace = max;
        self
    }

    /// The default limits with another per-route session cap.
    #[must_use]
    pub fn with_max_sessions_per_route(mut self, max: usize) -> Self {
        self.max_sessions_per_route = max;
        self
    }

    /// The default limits with other resolve and connect timeouts.
    #[must_use]
    pub fn with_network_timeouts(mut self, resolve: Duration, connect: Duration) -> Self {
        self.resolve_timeout = resolve;
        self.connect_timeout = connect;
        self
    }

    /// The default limits with other timeouts for terminated connections: TLS handshake (both
    /// sides), idle between requests, wait for an upstream answer, stall inside a body.
    #[must_use]
    pub fn with_terminate_timeouts(
        mut self,
        handshake: Duration,
        keepalive: Duration,
        upstream_head: Duration,
        body_idle: Duration,
    ) -> Self {
        self.tls_handshake_timeout = handshake;
        self.guest_handshake_timeout = handshake;
        self.keepalive_timeout = keepalive;
        self.upstream_head_timeout = upstream_head;
        self.body_idle_timeout = body_idle;
        self
    }

    /// The default limits with another budget for the guest's TLS handshake only.
    #[must_use]
    pub fn with_guest_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.guest_handshake_timeout = timeout;
        self
    }

    /// The default limits with another number of plain-HTTP requests per guest connection.
    #[must_use]
    pub fn with_max_requests_per_connection(mut self, max: usize) -> Self {
        self.max_requests_per_connection = max.max(1);
        self
    }

    /// The default limits with other agent-session limits.
    #[must_use]
    pub fn with_session(mut self, session: HostConfig) -> Self {
        self.session = session;
        self
    }
}
