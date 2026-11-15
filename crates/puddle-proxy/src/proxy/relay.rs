// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decide, connect and relay one `CONNECT` whose destination is known.

use std::sync::Arc;

use puddle_agent_proto::host::GuestStream;
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionOutcome, ConnectionReason,
    EgressRequest, ProtocolHint,
};
use tokio::net::TcpStream;

use crate::counted::Counted;
use crate::http::Head;
use crate::opened::Opened;
use crate::ssh;
use crate::target::Target;
use crate::terminate::{self};
use crate::token_gate::{self};
use crate::upstream::connect_out;
use puddle_upstream::RouteWatch;

use super::admit::note_block;
use super::{
    GuestReader, Proxy, WorkspaceHandler, admit, apply_reason, refuse, refuse_by_request_rule,
    tunnel,
};

/// The port SSH listens on.
const GIT_SSH_PORT: u16 = 22;

/// Decides, connects and relays one `CONNECT` whose destination is known; returns what the audit
/// records about it (bytes are filled in by the caller once the stream is gone). Plain HTTP goes
/// to [`plain::serve`] instead.
pub(super) async fn relay(
    handler: &WorkspaceHandler,
    mut reader: GuestReader,
    request: &EgressRequest,
    head: &Head,
    target: &Target,
) -> ConnectionEvent {
    let proxy = &handler.proxy;
    let from_container = ssh::from_container(head);
    // A client that waits for the server's identification before it sends its own (`ssh-keyscan`)
    // sends nothing that marks it as SSH. Port 22 of a Git host whose SSH Puddle serves is SSH, so
    // such a client gets Puddle's server and its keys, as `ssh` does.
    if (request.protocol == Some(ProtocolHint::Ssh) || request.port == GIT_SSH_PORT)
        && let Some((git, termination, host)) = proxy.git_ssh_for(request)
    {
        return crate::ssh_git::serve(handler, reader, request, git, &termination, host).await;
    }
    let (admitted, mut event) = admit(proxy, request).await;
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            refuse(reader.get_mut(), &refusal).await;
            return event;
        }
    };
    if let Some(refused) = refuse_by_request_rule(proxy, request, head, None) {
        let (refusal, change) = refused;
        change(&mut event);
        refuse(reader.get_mut(), &refusal).await;
        return event;
    }
    let mut guest = Opened::from(reader);
    let terminating = proxy.terminating(request, from_container);
    // SSH to a name that is not a Git host is spliced, even on port 443 of a decrypted name. The
    // header that says SSH is the guest's word, so there the first bytes decide: an identification
    // line is spliced, anything else is decrypted like every connection to the name.
    if terminating.is_some() && request.protocol == Some(ProtocolHint::Ssh) {
        guest.read_first(proxy.config.head_timeout).await;
    }
    // A bound host is terminated: the guest's TLS handshake comes first, so a client that asks
    // for the wrong name never causes a connection to the real server.
    if let Some((termination, mode, tls)) = terminating.filter(|_| !ssh::started(&guest.early)) {
        let cx = Arc::new(terminate::Context {
            proxy: Arc::clone(proxy),
            termination,
            mode,
            tls: tls.clone(),
            workspace: request.workspace.clone(),
            target: target.clone(),
            admitted,
            connections: Arc::clone(&handler.streams),
            streams: Arc::clone(&handler.h2_streams),
        });
        let done = terminate::run(cx, guest).await;
        apply_reason(&mut event, &done);
        event.http = done.http;
        if let Some(refused) = &done.refused_by_rule {
            event.http = Some(refused.line.clone());
        }
        event.resolved_ip = done.resolved_ip;
        event.upstream = done.hop;
        event.outcome = done.connect;
        event.injected = done.injected;
        event.binding_id = done.binding_id;
        event.unswapped_stand_ins = done.unswapped_stand_ins;
        event.scrubbed_stand_ins = done.scrubbed_stand_ins;
        event.foreign_credentials = done.foreign_credentials;
        event.answer_credentials = done.answer_credentials;
        if done.ssh {
            note_block(&mut event, BlockReason::SshUnsupported);
        }
        return event;
    }
    let out = match connect_out(
        proxy.upstream.as_ref(),
        target,
        true,
        &admitted,
        proxy.config.connect_timeout,
    )
    .await
    {
        Ok(out) => out,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            // A guest that has its `200` already gets no second answer: the connection ends.
            if !guest.answered {
                refuse(&mut guest.stream, &refusal).await;
            }
            return event;
        }
    };
    event.outcome = Some(ConnectionOutcome::Connected);
    event.resolved_ip = out.addr.map(|addr| addr.ip());
    event.upstream = out.hop;
    if let Some(code) = proxy.skipped(request, from_container) {
        event.reason = ConnectionReason::PassedThrough(code);
    }
    note_tunnel(proxy, guest, (out.stream, out.watch), request, &mut event).await;
    event
}

/// Splices a `CONNECT` tunnel that is not decrypted, and puts on `event` what it saw.
async fn note_tunnel(
    proxy: &Proxy,
    guest: Opened<Counted<GuestStream>>,
    (stream, watch): (TcpStream, Option<RouteWatch>),
    request: &EgressRequest,
    event: &mut ConnectionEvent,
) {
    let termination = proxy.termination_of(request);
    let kex = proxy.kex_watch(request, termination.as_deref());
    let end = tunnel(
        guest,
        stream,
        Some(request).filter(|request| proxy.is_git_ssh_name(request)),
        watch,
        termination,
        kex,
    )
    .await;
    event.http = end.http;
    if end.ssh {
        note_block(event, BlockReason::SshUnsupported);
    }
    if end.token_endpoint {
        event.decision = ConnectionDecision::Blocked;
        event.reason = ConnectionReason::Refused(token_gate::CODE);
    }
}
