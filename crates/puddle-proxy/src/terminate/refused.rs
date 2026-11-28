// SPDX-License-Identifier: AGPL-3.0-or-later
//! A refused `CONNECT` answered inside the tunnel.
//!
//! libcurl (curl, cargo, rustup) drops the reason phrase and the body of a `CONNECT` that is not
//! a `2xx`, so a tool behind a refusal prints only "CONNECT tunnel failed, response 403" and
//! cargo retries it for minutes. A refusal that is a decision (a pending approval, a deny) is
//! therefore sent as the answer to the tool's first HTTPS request instead: the guest gets the
//! `200`, a handshake with the workspace CA's leaf for the name it asked for (the guest trusts
//! that CA), and a `403` whose body names the host and what to do. Nothing reaches the real
//! server; the decision is the one the rules made.

use std::sync::Arc;

use puddle_ca::WorkspaceCa;
use puddle_types::Host;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use super::guest::{ALPN_HTTP11, Prefixed, read_hello, server_config};
use crate::http;
use crate::opened::Opened;
use crate::proxy::{ProxyConfig, Refusal, refuse};

/// Whether a refusal may be answered inside the tunnel: only a rule's decision (`x-puddle-decision`
/// is `pending` or `deny`), never a failure or an address block.
pub(crate) fn tellable(refusal: &Refusal) -> bool {
    refusal.status.starts_with("403")
        && refusal.headers.iter().any(|(name, value)| {
            *name == "x-puddle-decision" && matches!(value.as_str(), "pending" | "deny")
        })
}

/// Answers `guest`'s `CONNECT` with `200`, finishes a TLS handshake for `host` with a leaf from
/// `ca`, reads the first request's head and answers it with `refusal`. A client that asks for
/// another name, offers only `h2`, or does not trust the CA gets a closed connection.
pub(crate) async fn serve<S>(
    config: &ProxyConfig,
    ca: &Arc<WorkspaceCa>,
    host: &Host,
    mut guest: Opened<S>,
    refusal: &Refusal,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    if let Err(err) = guest.answer().await {
        tracing::debug!(error = %err, "guest went away before the TLS handshake");
        return;
    }
    let Opened { stream, early, .. } = guest;
    let read = tokio::time::timeout(
        config.guest_handshake_timeout,
        read_hello(Prefixed::new(stream, early), host),
    )
    .await;
    let Ok(Ok((start, hello))) = read else {
        tracing::debug!(%host, "guest TLS hello not read");
        return;
    };
    if !hello.name_matches || !hello.offers_h1 {
        tracing::info!(%host, "refusal not told inside the tunnel: another name or no HTTP/1.1");
        return;
    }
    let Ok((server, _)) = server_config(ca, host, &[ALPN_HTTP11]) else {
        return;
    };
    let tls = match tokio::time::timeout(config.guest_handshake_timeout, start.into_stream(server))
        .await
    {
        Ok(Ok(tls)) => tls,
        Ok(Err(err)) => {
            tracing::info!(%host, error = %err, "the client did not take Puddle's certificate for a refused host, so it sees a TLS error");
            return;
        }
        Err(_) => return,
    };
    let mut reader = BufReader::new(tls);
    // The head is read and dropped, so that the answer is not lost to a reset of unread data.
    let _ = tokio::time::timeout(config.head_timeout, http::read_head(&mut reader)).await;
    refuse(reader.get_mut(), refusal).await;
}
