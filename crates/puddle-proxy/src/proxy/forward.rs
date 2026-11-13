// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tunnelling and forwarding once a connection is admitted.

use std::io;
use std::sync::Arc;

use puddle_agent_proto::relay::splice;
use puddle_types::{BlockReason, EgressRequest, HttpRequestLine};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use puddle_netpolicy::block_message;

use crate::http::{self, Body, Head};
use crate::opened::{ESTABLISHED, Opened};
use crate::ssh;
use crate::tap::RequestTap;
use crate::target::Target;
use crate::terminate::{self};
use crate::token_gate::{self, TokenGate};
use crate::upstream::ProxyForm;
use puddle_upstream::RouteWatch;

use super::ClientReader;

pub(crate) use puddle_upstream::connect_first;

/// What a [`tunnel`] saw.
#[derive(Debug, Default)]
pub(crate) struct TunnelEnd {
    /// The tunnel's first request line, if it carried plain HTTP/1.x.
    pub(crate) http: Option<HttpRequestLine>,
    /// The guest started SSH: nothing of it was passed on, and the guest got the refusal.
    pub(crate) ssh: bool,
    /// The guest's `ClientHello` asked for a token endpoint of the workspace's logins: it was not
    /// passed on, and the guest got a TLS alert.
    pub(crate) token_endpoint: bool,
}

/// `CONNECT`: answer `200` (unless `guest` was answered already), pass on bytes the guest sent
/// early, splice. On an error both ends reset: [`splice`] sets zero linger on the server socket,
/// and the guest stream is dropped without a shutdown.
///
/// Reports the tunnel's first request line if it carried plain HTTP/1.x (Node `fetch` and Yarn
/// Berry tunnel `http://` this way), for the audit. The bytes are relayed unchanged.
///
/// With `ssh_for` (the tunnel's request), a tunnel whose first upload bytes are an SSH
/// identification line is ended instead: the line is not passed on, the guest gets the SSH
/// refusal and a clean close, and the server a reset ([`ssh::Gate`]). That is how SSH to a host a
/// rule allows is still refused, on any port.
pub(crate) async fn tunnel<S: AsyncRead + AsyncWrite + Unpin>(
    guest: Opened<S>,
    mut server: TcpStream,
    ssh_for: Option<&EgressRequest>,
    mut watch: Option<RouteWatch>,
    token_endpoints: Option<Arc<terminate::Termination>>,
    kex: Option<crate::ssh_agent::KexWatch>,
) -> TunnelEnd {
    let Opened {
        stream,
        early,
        answered,
    } = guest;
    let tapped = TokenGate::new(RequestTap::new(stream, &early), token_endpoints, &early);
    // The gate hands `early` on first, so it reaches the server through the splice, after the
    // gate has looked at it.
    let mut guest = ssh::Gate::new(tapped, early, ssh_for.is_some()).with_kex(kex);
    if guest.inner().refused() {
        abort(&server);
        return refuse_token_endpoint(&mut guest).await;
    }
    if !answered && let Err(err) = guest.write_all(ESTABLISHED).await {
        tracing::debug!(error = %err, "tunnel failed before the splice");
        abort(&server);
        return seen_only(&guest);
    }
    // A tunnel can outlive the network it was made on. When the route to its destination changes
    // so that the hop it uses is no longer on it, both ends are reset (the same ending as any
    // failed tunnel), and the guest's client reconnects over the new route.
    let spliced = splice_watched(&mut server, &mut guest, watch.as_mut()).await;
    let Some(spliced) = spliced else {
        abort(&server);
        return seen_only(&guest);
    };
    match spliced {
        Ok((up, down)) => tracing::debug!(bytes_up = up, bytes_down = down, "tunnel closed"),
        Err(err) => {
            if let Some(request) = ssh_for.filter(|_| guest.ssh_seen()) {
                tracing::info!(host = %request.host, port = request.port, "ssh in a tunnel refused");
                let message = block_message(
                    &request.host,
                    &request.workspace,
                    &[BlockReason::SshUnsupported],
                );
                let _ = guest.write_all(&ssh::pre_banner(&message)).await;
                let _ = guest.shutdown().await;
                return TunnelEnd {
                    ssh: true,
                    ..TunnelEnd::default()
                };
            }
            if guest.inner().refused() {
                return refuse_token_endpoint(&mut guest).await;
            }
            tracing::debug!(error = %err, "tunnel aborted");
        }
    }
    seen_only(&guest)
}

/// What a tunnel that ended without a refusal saw: its first request line, if it carried plain HTTP.
fn seen_only<S>(guest: &ssh::Gate<TokenGate<RequestTap<S>>>) -> TunnelEnd {
    TunnelEnd {
        http: guest.inner().inner().request_line(),
        ..TunnelEnd::default()
    }
}

/// Ends a tunnel whose hello asked for a token endpoint: a TLS alert to the guest, a clean close.
async fn refuse_token_endpoint<S: AsyncRead + AsyncWrite + Unpin>(
    guest: &mut ssh::Gate<TokenGate<S>>,
) -> TunnelEnd {
    tracing::info!("tunnel refused: the TLS hello asks for a token endpoint");
    let _ = guest.write_all(&token_gate::ALERT).await;
    let _ = guest.shutdown().await;
    TunnelEnd {
        token_endpoint: true,
        ..TunnelEnd::default()
    }
}

/// [`splice`] until either side ends, or until `watch` says the route to the destination
/// changed (`None`: both ends are to be reset by the caller).
pub(crate) async fn splice_watched<S>(
    server: &mut TcpStream,
    guest: &mut S,
    watch: Option<&mut RouteWatch>,
) -> Option<io::Result<(u64, u64)>>
where
    S: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    let mut spliced = std::pin::pin!(splice(server, guest));
    match watch {
        Some(watch) => tokio::select! {
            result = &mut spliced => Some(result),
            () = watch.changed() => None,
        },
        None => Some(spliced.await),
    }
}

/// One plain-HTTP request on a connection that carries only it (the image-pull listener's way;
/// a workspace's plain HTTP keeps its connection, see [`plain::serve`]): `Host` rewritten to the
/// checked target, body framed exactly, so a second request can't ride on the checked connection.
pub(crate) async fn forward<S: AsyncRead + AsyncWrite + Unpin>(
    mut reader: ClientReader<S>,
    mut server: TcpStream,
    head: &Head,
    path: &str,
    target: &Target,
    body: Body,
    via: Option<&ProxyForm>,
) {
    // To a proxy the request target is the absolute URI (of the name or the checked address the
    // proxy was told), with the proxy credential for this request if the scheme needs one.
    let uri = via.map_or_else(
        || path.to_owned(),
        |proxy| format!("http://{}{path}", proxy.authority),
    );
    let authorization = via.and_then(|p| {
        p.authorization
            .as_ref()
            .map(puddle_upstream::Secret::expose)
    });
    let upstream_head = http::upstream_head(head, &uri, &target.host_header(), authorization);
    let result = async {
        server.write_all(upstream_head.as_bytes()).await?;
        forward_one_request(&mut reader, &mut server, body).await
    }
    .await;
    match result {
        Ok(0) => tracing::debug!("request forwarded"),
        Ok(extra) => tracing::debug!(
            discarded_bytes = extra,
            "request forwarded; pipelined bytes discarded"
        ),
        Err(err) => {
            tracing::debug!(error = %err, "plain-HTTP request aborted");
            abort(&server);
            // `reader` is dropped without a shutdown: the guest stream resets.
        }
    }
}

/// Sends the body up while the response comes down (so `Expect: 100-continue` works). Anything
/// the guest sends after the body is never forwarded; returns how many such bytes were buffered.
pub(super) async fn forward_one_request<C, U>(
    client: &mut BufReader<C>,
    upstream: &mut U,
    body: Body,
) -> io::Result<usize>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut server_in, mut server_out) = tokio::io::split(upstream);
    let leftover = {
        let (guest_in, mut guest_out) = tokio::io::split(&mut *client);
        let mut guest_in = BufReader::new(guest_in);
        {
            let upload = async {
                http::copy_body(&mut guest_in, &mut server_out, body).await?;
                server_out.flush().await
            };
            let download = tokio::io::copy(&mut server_in, &mut guest_out);
            tokio::pin!(download);
            let uploaded_first = tokio::select! {
                up = upload => { up?; true }
                down = &mut download => { down?; false }
            };
            if uploaded_first {
                download.await?;
            }
        }
        guest_out.shutdown().await?;
        guest_in.buffer().len()
    };
    Ok(leftover + client.buffer().len())
}

/// Makes dropping `server` send a reset instead of a clean close.
pub(crate) fn abort(server: &TcpStream) {
    if let Err(err) = server.set_zero_linger() {
        tracing::debug!(error = %err, "zero linger");
    }
}
