// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decide, connect and relay one `CONNECT` whose destination is known.

use std::sync::Arc;

use puddle_agent_proto::host::GuestStream;
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionOutcome, ConnectionReason,
    EgressRequest,
};
use tokio::net::TcpStream;

use crate::counted::Counted;
use crate::http::Head;
use crate::opened::Opened;
use crate::request_target::Target;
use crate::ssh;
use crate::terminate::{self};
use crate::token_gate::{self};
use crate::upstream::connect_out;
use puddle_upstream::RouteWatch;

use super::admit::{note_block, refuse_not_running, refuse_ssh};
use super::plan::{Admitted, ConnectionPlan, GuestHints, Splice};
use super::{GuestReader, Proxy, WorkspaceHandler, admit, apply_reason, refuse, tunnel};

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
    let seen = proxy.seen(&request.workspace);
    let plan = ConnectionPlan::new(proxy, &seen, request, head, GuestHints::of(request, head));
    let plan = match plan {
        ConnectionPlan::GitSsh {
            git,
            termination,
            host,
        } => {
            return crate::ssh_git::serve(handler, reader, request, git, &termination, host).await;
        }
        ConnectionPlan::NotRunning => {
            return refused(&mut reader, refuse_not_running(request)).await;
        }
        ConnectionPlan::SshRefused => {
            return refused(&mut reader, refuse_ssh(proxy, request).await).await;
        }
        ConnectionPlan::Admit(plan) => plan,
    };
    let (admitted, mut event) = admit(proxy, request).await;
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            refuse(reader.get_mut(), &refusal).await;
            return event;
        }
    };
    let (mut guest, splice) = match plan {
        Admitted::RuleRefusesPort(refusal, change) => {
            change(&mut event);
            refuse(reader.get_mut(), &refusal).await;
            return event;
        }
        Admitted::Splice(splice) => (Opened::from(reader), splice),
        Admitted::Terminate {
            termination,
            mode,
            tls,
            ssh_first,
            splice,
        } => {
            // SSH to a name that is not a Git host is spliced, even on port 443 of a decrypted
            // name. The header that says SSH is the guest's word, so there the first bytes decide:
            // an identification line is spliced, anything else is decrypted like every
            // connection to the name.
            let mut guest = Opened::from(reader);
            if ssh_first {
                guest.read_first(proxy.config.head_timeout).await;
            }
            if ssh::started(&guest.early) {
                (guest, splice)
            } else {
                // A bound host is terminated: the guest's TLS handshake comes first, so a client
                // that asks for the wrong name never causes a connection to the real server.
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
                return terminated(event, terminate::run(cx, guest).await);
            }
        }
    };
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
    if let Some(code) = splice.passed_through {
        event.reason = ConnectionReason::PassedThrough(code);
    }
    note_tunnel(
        proxy,
        guest,
        (out.stream, out.watch),
        request,
        splice,
        &mut event,
    )
    .await;
    event
}

/// Sends a refusal made before admission and returns the audit record that goes with it.
async fn refused(
    reader: &mut GuestReader,
    (refusal, event): (super::Refusal, ConnectionEvent),
) -> ConnectionEvent {
    refuse(reader.get_mut(), &refusal).await;
    event
}

/// Puts what a decrypted connection's end says on its audit record.
fn terminated(mut event: ConnectionEvent, done: terminate::Outcome) -> ConnectionEvent {
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
    event
}

/// Splices a `CONNECT` tunnel that is not decrypted, and puts on `event` what it saw.
async fn note_tunnel(
    proxy: &Proxy,
    guest: Opened<Counted<GuestStream>>,
    (stream, watch): (TcpStream, Option<RouteWatch>),
    request: &EgressRequest,
    splice: Splice,
    event: &mut ConnectionEvent,
) {
    let Splice {
        termination,
        git_ssh_name,
        ..
    } = splice;
    let kex = proxy.kex_watch(request, termination.as_deref());
    let end = tunnel(
        guest,
        stream,
        git_ssh_name.then_some(request),
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
