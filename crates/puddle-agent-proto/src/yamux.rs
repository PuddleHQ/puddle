// SPDX-License-Identifier: AGPL-3.0-or-later
//! yamux settings shared by the agent (client) and the host (server).

use std::time::Duration;

use tokio_yamux::Config;

/// Receive window per stream, both directions: 16 MiB.
///
/// With the agent's vsock buffer raised past msb's 8 MiB (`puddle-agent`'s vsock module),
/// this window sets per-stream speed only; a single 1 GiB upload went from 68 to 334 MiB/s when it
/// went from 256 KiB back to 16 MiB.
pub const STREAM_WINDOW: u32 = 16 * 1024 * 1024;

/// Streams the host lets wait for its accept loop. A parallel restore or `docker pull` opens
/// hundreds at once; the default of 256 would refuse some.
pub const ACCEPT_BACKLOG: usize = 4096;

/// Streams one session may have open at once. Hundreds at once is a busy day (a parallel restore,
/// a `docker pull`); every stream past this is reset, so a guest cannot make the host keep a task
/// and its buffers for each of 65,535 streams that sit open for the 30 s a silent one lives.
pub const MAX_STREAMS: usize = 4096;

/// Stream data the host's session holds on its way to the socket, summed over all its streams: 32 MiB,
/// two stream windows. A stream's write waits once it is reached and goes on as the socket takes data.
/// Without it a guest that grants every stream a window and reads none makes the host queue
/// `MAX_STREAMS` x `STREAM_WINDOW` bytes.
pub const SEND_BUDGET: usize = 2 * STREAM_WINDOW as usize;

/// The host's keepalive ping interval: a liveness check only, it plays no part in flow control.
pub const KEEPALIVE: Duration = Duration::from_secs(30);

/// First byte of every yamux frame (protocol version 0). An HTTP request line starts with a
/// letter, so the host can tell a yamux session from anything else by this byte.
pub const VERSION_BYTE: u8 = 0;

/// The agent's session settings.
#[must_use]
pub fn client_config() -> Config {
    client_config_with_window(STREAM_WINDOW)
}

/// The agent's session settings with another receive window (`PUDDLE_AGENT_WINDOW`, for
/// experiments). yamux needs at least 256 KiB; smaller values are raised to that.
#[must_use]
pub fn client_config_with_window(window: u32) -> Config {
    Config {
        max_stream_window_size: window.max(tokio_yamux::config::INITIAL_STREAM_WINDOW),
        ..Config::default()
    }
}

/// The host's session settings.
#[must_use]
pub fn server_config() -> Config {
    Config {
        accept_backlog: ACCEPT_BACKLOG,
        max_stream_count: MAX_STREAMS,
        max_stream_window_size: STREAM_WINDOW,
        max_send_budget: SEND_BUDGET,
        // Keepalive is on at `KEEPALIVE` by the vendored default; the test pins both.
        ..Config::default()
    }
}

#[cfg(test)]
mod tests;
