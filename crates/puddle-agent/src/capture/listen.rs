// SPDX-License-Identifier: GPL-3.0-or-later
//! The transparent listener: where the capture rules send a connection.
//!
//! A connection the rules redirected here has lost its destination, but the kernel remembers it
//! (`SO_ORIGINAL_DST`). The listener finds the name behind a stand-in address in the table the
//! stub DNS keeps (or uses the address itself for a real one), waits a moment for the client's
//! first bytes, and sends `CONNECT name:port` to the host as an explicit proxy client would, with
//! those bytes right behind it so the proxy can tell SSH before it decides. The proxy's answer
//! decides what the client sees: `200` and the tunnel, or a reset.
//!
//! A refused SSH client (libssh2, Go: clients that never read `ssh_config`) is told why before it
//! is closed: the proxy's text goes out as lines ahead of the identification string, which a server
//! may send (RFC 4253 section 4.2), and the client shows what its library shows of them.

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::relay::splice;
use puddle_agent_proto::ssh::is_banner;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

use super::table::{StandIns, in_range};
use crate::connect::{connect_request, peek, read_answer};
use crate::upstream::Upstream;

/// How long a client gets to send its first bytes before the connection goes up without them.
pub const FIRST_BYTES: Duration = Duration::from_millis(50);
/// How long the proxy may take to answer (it connects to the destination first).
const ANSWER: Duration = Duration::from_secs(60);
/// Pause after a failed accept (e.g. out of file descriptors).
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

/// What a connection was for, once its original destination is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A name the stub DNS gave a stand-in for.
    Name(String, u16),
    /// A real address a tool connected to directly.
    Address(Ipv4Addr, u16),
    /// A stand-in address nothing knows a name for (the table never held it).
    Unknown,
}

impl Destination {
    /// The `CONNECT` target, `None` for [`Destination::Unknown`].
    #[must_use]
    pub fn authority(&self) -> Option<String> {
        match self {
            Self::Name(name, port) => Some(format!("{name}:{port}")),
            Self::Address(ip, port) => Some(format!("{ip}:{port}")),
            Self::Unknown => None,
        }
    }
}

/// Maps the connection's original destination to what to ask the proxy for. The returned hold
/// keeps the stand-in's name from being given to another name while the connection lives.
#[must_use]
pub fn destination(
    original: SocketAddrV4,
    table: &StandIns,
) -> (Destination, Option<super::table::Hold>) {
    let (ip, port) = (*original.ip(), original.port());
    if !in_range(ip) {
        return (Destination::Address(ip, port), None);
    }
    match table.hold(ip) {
        Some(hold) => (Destination::Name(hold.name().to_owned(), port), Some(hold)),
        None => (Destination::Unknown, None),
    }
}

/// Accepts redirected connections forever.
pub async fn serve(listener: TcpListener, table: StandIns, upstream: Arc<Upstream>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((client, _)) => {
                    conns.spawn(connection(client, table.clone(), Arc::clone(&upstream)));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "capture accept failed");
                    tokio::time::sleep(ACCEPT_PAUSE).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// Whether a connection whose conntrack entry says `original` was redirected here: conntrack
/// answers for any tracked connection, so one dialled straight at the port "originally" went to the
/// address it arrived on. Loopback is never redirected (the rules leave it alone).
#[must_use]
pub fn was_redirected(original: SocketAddrV4, arrived_on: Option<std::net::SocketAddr>) -> bool {
    !original.ip().is_loopback() && arrived_on != Some(std::net::SocketAddr::V4(original))
}

/// A reset, so the client sees a failure rather than an empty response.
fn reset(client: &TcpStream) {
    if let Err(err) = client.set_zero_linger() {
        tracing::debug!(error = %err, "zero linger on a refused capture connection");
    }
}

async fn connection(mut client: TcpStream, table: StandIns, upstream: Arc<Upstream>) {
    // A connection nobody redirected (a process dialling the listener's port itself) has no
    // original destination: there is nothing to ask the proxy for.
    let redirected = rustix::net::sockopt::ip_original_dst(&client)
        .ok()
        .filter(|original| was_redirected(*original, client.local_addr().ok()));
    let Some(original) = redirected else {
        tracing::debug!("connection to the capture port that no rule redirected");
        reset(&client);
        return;
    };
    let (dest, _hold) = destination(original, &table);
    let Some(authority) = dest.authority() else {
        tracing::warn!(%original, "stand-in address with no name in the table (it was lost); connection reset");
        reset(&client);
        return;
    };
    let first = peek(&mut client, FIRST_BYTES).await;
    let mut stream = match upstream.open().await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "no stream to the host; captured connection reset");
            reset(&client);
            return;
        }
    };
    if let Err(err) = stream_write(&mut stream, &connect_request(&authority, &first)).await {
        tracing::debug!(error = %err, "writing the CONNECT failed");
        reset(&client);
        return;
    }
    let mut reader = BufReader::new(stream);
    let answer = tokio::time::timeout(ANSWER, read_answer(&mut reader)).await;
    match answer {
        Ok(Ok((200, _))) => {
            let rest = reader.buffer().to_vec();
            let mut stream = reader.into_inner();
            if client.write_all(&rest).await.is_err() {
                return;
            }
            if let Err(err) = splice(&mut client, &mut stream).await {
                tracing::debug!(error = %err, "captured connection ended with an error");
            }
        }
        Ok(Ok((status, body))) => {
            tracing::debug!(%authority, status, "the proxy refused a captured connection");
            if is_banner(&first) {
                tell_ssh_client(&mut client, &body).await;
            } else {
                reset(&client);
            }
        }
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "no answer from the proxy for a captured connection");
            reset(&client);
        }
        Err(_) => {
            tracing::debug!(%authority, "the proxy took too long; captured connection reset");
            reset(&client);
        }
    }
}

async fn stream_write<W: tokio::io::AsyncWrite + Unpin>(
    w: &mut W,
    bytes: &[u8],
) -> std::io::Result<()> {
    w.write_all(bytes).await?;
    w.flush().await
}

/// Sends the refusal as lines ahead of any identification string, then closes cleanly so the
/// client reads them before it sees the end.
async fn tell_ssh_client(client: &mut TcpStream, body: &[u8]) {
    let text = String::from_utf8_lossy(body);
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        // A line must not itself look like an identification string.
        let line = line.trim_start_matches("SSH-");
        out.push_str(line);
        out.push_str("\r\n");
    }
    let _ = client.write_all(out.as_bytes()).await;
    let _ = client.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_connection_dialled_at_the_port_was_not_redirected() {
        let at = |s: &str| Some(s.parse::<std::net::SocketAddr>().unwrap());
        let orig = |s: &str| s.parse::<SocketAddrV4>().unwrap();
        // Arrived on the address it was "originally" going to: nobody redirected it.
        assert!(!was_redirected(
            orig("172.17.0.1:3129"),
            at("172.17.0.1:3129")
        ));
        assert!(!was_redirected(
            orig("127.0.0.1:3129"),
            at("127.0.0.1:3129")
        ));
        // Loopback is never a redirect target, even if the addresses differ.
        assert!(!was_redirected(orig("127.0.0.2:80"), at("127.0.0.1:3129")));
        // Redirected: the original destination is somewhere else.
        assert!(was_redirected(
            orig("93.184.216.34:443"),
            at("127.0.0.1:3129")
        ));
        assert!(was_redirected(
            orig("198.18.0.9:443"),
            at("172.17.0.1:3129")
        ));
        assert!(was_redirected(orig("93.184.216.34:443"), None));
    }

    #[test]
    fn a_real_address_is_asked_for_by_address() {
        let table = StandIns::in_memory();
        let (dest, hold) = destination("93.184.216.34:443".parse().unwrap(), &table);
        assert_eq!(
            dest,
            Destination::Address(Ipv4Addr::new(93, 184, 216, 34), 443)
        );
        assert_eq!(dest.authority().as_deref(), Some("93.184.216.34:443"));
        assert!(hold.is_none());
    }

    #[test]
    fn a_stand_in_is_asked_for_by_name_and_held_while_the_connection_lives() {
        let table = StandIns::in_memory();
        let ip = table.address_for("db.example.com").unwrap();
        let (dest, hold) = destination(SocketAddrV4::new(ip, 5432), &table);
        assert_eq!(dest.authority().as_deref(), Some("db.example.com:5432"));
        assert_eq!(hold.unwrap().name(), "db.example.com");
    }

    #[test]
    fn a_stand_in_nobody_handed_out_is_unknown_not_a_guess() {
        let table = StandIns::in_memory();
        let (dest, hold) = destination("198.18.5.5:443".parse().unwrap(), &table);
        assert_eq!(dest, Destination::Unknown);
        assert_eq!(dest.authority(), None);
        assert!(hold.is_none());
    }
}
