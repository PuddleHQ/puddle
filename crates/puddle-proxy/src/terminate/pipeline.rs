// SPDX-License-Identifier: AGPL-3.0-or-later
//! The steps every terminated request goes through, whichever HTTP version the guest speaks.
//!
//! The request side ([`admit`], then [`prepare`]) notes the request line, asks the rules on method
//! and path, reads the body when the injector or the exchange wants it, asks the injector, builds
//! the upstream headers, swaps stand-ins in the headers, the target and an exchange's body, and
//! begins the exchange. The answer side ([`replaced_401`], then [`answer`]) replaces a `401`,
//! lets the exchange read the answer, scrubs it and scans it for unknown tokens.
//!
//! What differs per protocol stays with the protocol: framing, `Expect: 100-continue`, the
//! upstream connection or route, the WebSocket splice and the response write. The one protocol
//! step inside the pipeline is reading the body, through [`Wire`].
//!
//! Nothing here touches the connection's [`Outcome`]: the request side fills a [`RequestAudit`]
//! that the caller folds in once with [`Outcome::absorb`], so the HTTP/2 path never holds its
//! outcome lock across an await.

use std::borrow::Cow;
use std::sync::Arc;

use ::http::header::{self, HeaderMap};
use ::http::{Method, Response, StatusCode};
use bytes::Bytes;
use hyper::body::Incoming;
use puddle_types::{ForeignCredentialKind, HttpRequestLine};
use zeroize::Zeroizing;

use super::exchange::{
    AnswerBody, AnswerError, Exchange, MAX_EXCHANGE_BODY, read_answer, swap_request_body,
};
use super::inject::{InjectContext, Injection, RequestView, Unauthorized};
use super::request::{self, UpstreamVersion};
use super::request_rules::{self, Refused};
use super::session::{Context, Outcome, stale_kind};
use super::stand_in::{
    AnswerReport, NotScrubbed, ScrubNotes, Swapped, answer_encoding_refusal, answer_failed_refusal,
    limit_accept_encoding, scan_answer, scrub_answer,
};
use crate::http::{self, Head};
use crate::proxy::Refusal;

/// How a protocol hands the pipeline the guest's request body.
pub(crate) trait Wire {
    /// Why a read stopped: at least a [`Refusal`] for the guest.
    type Stop: From<Refusal>;

    /// The guest's whole request body, at most `limit` bytes (`413` when it is larger).
    async fn read_body(&mut self, limit: usize) -> Result<Bytes, Self::Stop>;
}

/// The guest's headers, as its protocol parsed them.
#[derive(Clone, Copy)]
pub(crate) enum GuestHeaders<'a> {
    /// The lines of an HTTP/1.1 head.
    Head(&'a Head),
    /// The header map of an HTTP/2 request.
    Map(&'a HeaderMap),
}

impl<'a> GuestHeaders<'a> {
    /// The headers to send upstream, with the injected ones (see [`request::upstream_headers`]
    /// and [`request::upstream_headers_h2`]).
    fn upstream(
        self,
        cx: &Context,
        injection: Option<&Injection>,
        version: UpstreamVersion,
    ) -> Result<HeaderMap, Refusal> {
        match self {
            Self::Head(head) => request::upstream_headers(head, &cx.target, injection),
            Self::Map(map) => request::upstream_headers_h2(map, &cx.target, injection, version),
        }
    }

    /// The request's first `Content-Type`.
    fn content_type(self) -> Option<&'a str> {
        match self {
            Self::Head(head) => head
                .headers
                .iter()
                .find(|line| http::header_name(line) == "content-type")
                .map(|line| http::header_value(line)),
            Self::Map(map) => map
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
        }
    }
}

/// One checked request from the guest, whichever protocol it came over.
pub(crate) struct GuestRequest<'a> {
    pub(crate) method: &'a str,
    /// Path and query, starting with `/`.
    pub(crate) target: &'a str,
    /// The header lines the rules and the injector see (`name: value`, with the `Host`).
    pub(crate) lines: &'a [String],
    pub(crate) headers: GuestHeaders<'a>,
    /// An HTTP/2 extended `CONNECT` for a WebSocket: the rules also judge it as the `GET` it
    /// stands for, its body is never read and it begins no exchange.
    pub(crate) extended_connect: bool,
}

impl GuestRequest<'_> {
    /// The method and header lines the injector sees. A WebSocket over an extended `CONNECT`
    /// reaches an HTTP/1.1 server as a `GET` with `Upgrade` headers; the injector judges what the
    /// server will be sent.
    fn injector_view(&self, version: UpstreamVersion) -> (&str, Cow<'_, [String]>) {
        if self.extended_connect && version == UpstreamVersion::H1 {
            let mut lines = self.lines.to_vec();
            lines.push("connection: upgrade".to_owned());
            lines.push("upgrade: websocket".to_owned());
            return (Method::GET.as_str(), Cow::Owned(lines));
        }
        (self.method, Cow::Borrowed(self.lines))
    }
}

/// What one request did that the connection's record carries; folded in by
/// [`Outcome::absorb`] whether the request went on or was refused.
#[derive(Debug, Default)]
pub(crate) struct RequestAudit {
    /// The request line, its stand-ins as labels.
    http: Option<HttpRequestLine>,
    /// The rule on method and path that refused it.
    refused_by_rule: Option<Refused>,
    /// The binding of the injected credential.
    binding: Option<String>,
    /// What each stand-in swap did (headers, target, exchange body), in order.
    swaps: Vec<Swapped>,
    /// The kinds of credential it carried that Puddle did not add or swap.
    foreign: Vec<ForeignCredentialKind>,
}

impl Outcome {
    /// Folds one request's audit into the connection's record.
    pub(crate) fn absorb(&mut self, cx: &Context, audit: RequestAudit) {
        let RequestAudit {
            http,
            refused_by_rule,
            binding,
            swaps,
            foreign,
        } = audit;
        if self.http.is_none() {
            self.http = http;
        }
        if self.refused_by_rule.is_none() {
            self.refused_by_rule = refused_by_rule;
        }
        if let Some(binding) = binding {
            self.injected = true;
            self.binding_id.get_or_insert(binding);
        }
        for swapped in &swaps {
            self.record_stand_ins(&cx.target.host, swapped);
        }
        self.record_foreign(cx, &foreign);
    }
}

/// Notes the request line for the audit, then refuses the request when a rule on method and path
/// names it, before anything is sent upstream.
pub(crate) fn admit(
    cx: &Context,
    req: &GuestRequest<'_>,
    audit: &mut RequestAudit,
) -> Result<(), Refusal> {
    let host = &cx.target.host;
    let shown = cx.termination.stand_ins().redact_target(req.target);
    tracing::debug!(%host, method = %req.method, path = %puddle_types::request_path(&shown), "terminated request");
    audit.http = Some(HttpRequestLine::new(req.method, &shown));
    let rules = cx.termination.request_rules();
    let mut methods = vec![req.method];
    if req.extended_connect {
        // An HTTP/1.1 server is sent the `GET` it stands for, and a rule written for a WebSocket
        // upgrade on HTTP/1.1 names that.
        methods.push(Method::GET.as_str());
    }
    let Some(rule) = methods
        .into_iter()
        .find_map(|method| rules.refusing(host, &RequestView::new(method, req.target, req.lines)))
    else {
        return Ok(());
    };
    tracing::info!(%host, %rule, "request refused by a rule on method and path");
    audit.refused_by_rule = Some(Refused {
        rule,
        line: HttpRequestLine::new(req.method, &shown),
    });
    Err(request_rules::refusal(rule, req.method, host))
}

/// A request ready to send upstream.
pub(crate) struct Prepared {
    pub(crate) headers: HeaderMap,
    /// The target with its stand-ins swapped, when it held any.
    pub(crate) target: Option<Zeroizing<String>>,
    /// The body, when it was read in full (for the injector or the exchange).
    pub(crate) body: Option<Bytes>,
    pub(crate) exchange: Option<Exchange>,
    pub(crate) injection: Option<Injection>,
    pub(crate) unauthorized: Option<Unauthorized>,
}

/// The request side after [`admit`] and once the route to the server is known (`version`): reads
/// the body the injector wants, asks the injector, builds the upstream headers, swaps the
/// stand-ins and begins the exchange. `Err`: the request goes no further.
pub(crate) async fn prepare<W: Wire>(
    cx: &Context,
    wire: &mut W,
    req: &GuestRequest<'_>,
    version: UpstreamVersion,
    audit: &mut RequestAudit,
) -> Result<Prepared, W::Stop> {
    let host = &cx.target.host;
    let context = InjectContext {
        workspace: &cx.workspace,
        host,
    };
    let injector = cx.termination.injector();
    let (method, lines) = req.injector_view(version);
    let view = RequestView::new(method, req.target, &lines);
    let wanted = if req.extended_connect {
        None
    } else {
        injector.body_wanted(&context, &view)
    };
    let prefetched = match wanted {
        Some(limit) => Some(wire.read_body(limit).await?),
        None => None,
    };
    let decision = match &prefetched {
        Some(body) => injector.decide(&context, &view.with_body(body)).await,
        None => injector.decide(&context, &view).await,
    };
    let forwarding = decision.into_forwarding().map_err(|refusal| {
        tracing::info!(%host, code = %refusal.code(), "request refused by the credential rules");
        refusal.to_refusal()
    })?;
    let injection = forwarding.injection;
    let mut headers = req.headers.upstream(cx, injection.as_ref(), version)?;
    if let Some(injection) = &injection {
        audit.binding = Some(injection.binding_id().to_owned());
    }
    let stand_ins = cx.termination.stand_ins();
    let mut foreign =
        stand_ins.foreign_credentials(&headers, injection.as_ref(), forwarding.own_git_credential);
    let swapped = stand_ins.swap(&mut headers, host, injection.as_ref());
    let (target, in_target) = stand_ins.swap_target(req.target, host);
    foreign.extend(stale_kind(&[&swapped, &in_target]));
    audit.swaps.extend([swapped, in_target]);
    audit.foreign = foreign;
    if stand_ins.scrubs_answers_from(host) {
        limit_accept_encoding(&mut headers);
    }
    let mut prepared = Prepared {
        headers,
        target,
        body: prefetched,
        exchange: None,
        injection,
        unauthorized: forwarding.unauthorized,
    };
    if !req.extended_connect {
        begin_exchange(cx, wire, req, &mut prepared, audit).await?;
    }
    Ok(prepared)
}

/// Asks the workspace's exchange rewriter about the request. If it is a token exchange, gets it
/// ready: the answer will be read, so the upstream is asked for it unencoded; and the body is
/// read in full and has its stand-ins swapped for the real values when the exchange asks for that.
async fn begin_exchange<W: Wire>(
    cx: &Context,
    wire: &mut W,
    req: &GuestRequest<'_>,
    prepared: &mut Prepared,
    audit: &mut RequestAudit,
) -> Result<(), W::Stop> {
    let host = &cx.target.host;
    let began = cx.termination.exchanges().and_then(|rewriter| {
        rewriter.begin(host, req.method, puddle_types::request_path(req.target))
    });
    let Some(mut exchange) = began else {
        return Ok(());
    };
    if exchange.answer_mut().is_some() {
        prepared.headers.remove(header::ACCEPT_ENCODING);
    }
    if exchange.wants_request_body() {
        let body = match prepared.body.take() {
            Some(body) => body,
            None => wire.read_body(MAX_EXCHANGE_BODY).await?,
        };
        let (swapped, report) = swap_request_body(
            cx.termination.stand_ins(),
            &exchange,
            req.headers.content_type(),
            &body,
            host,
        );
        audit.swaps.push(report);
        prepared.body = Some(swapped.unwrap_or(body));
    }
    prepared.exchange = Some(exchange);
    Ok(())
}

/// The refusal that replaces the server's answer when it is a `401` for a request whose
/// credential was Puddle's to choose.
pub(crate) fn replaced_401(
    cx: &Context,
    status: StatusCode,
    unauthorized: Option<&Unauthorized>,
) -> Option<Refusal> {
    if status != StatusCode::UNAUTHORIZED {
        return None;
    }
    let refusal = unauthorized?.refusal()?;
    tracing::info!(host = %cx.target.host, code = %refusal.code(), "401 replaced: the credential for this request was Puddle's to choose");
    Some(refusal.to_refusal())
}

/// The server's answer as the guest gets it: read by the exchange when it has a rewriter, its
/// real values put back as stand-ins (`notes` records which) and scanned for tokens Puddle does
/// not hold (`unknown`). `Err`: the refusal the guest gets instead; the upstream connection is
/// not to be used again.
pub(crate) async fn answer(
    cx: &Context,
    exchange: Option<&mut Exchange>,
    response: Response<Incoming>,
    head_only: bool,
    notes: &Arc<ScrubNotes>,
    unknown: &Arc<AnswerReport>,
) -> Result<Response<AnswerBody>, Refusal> {
    let host = &cx.target.host;
    let idle = cx.proxy.config.body_idle_timeout;
    let response = match exchange.and_then(Exchange::answer_mut) {
        None => response.map(AnswerBody::Streaming),
        Some(rewriter) => read_answer(rewriter, response, idle)
            .await
            .map_err(|err| match err {
                AnswerError::Refused(refusal) => {
                    tracing::info!(%host, code = %refusal.code(), "the answer was refused");
                    refusal.to_refusal()
                }
                AnswerError::Upstream(err) => {
                    tracing::info!(%host, error = %err, "the answer could not be read");
                    Refusal::new(
                        "502 Bad Gateway",
                        format!("{host} did not finish its answer"),
                    )
                }
            })?,
    };
    let stand_ins = cx.termination.stand_ins();
    let scrubbed = scrub_answer(stand_ins, response, host, head_only, notes, idle)
        .await
        .map_err(|why| match why {
            NotScrubbed::Encoding => answer_encoding_refusal(host),
            NotScrubbed::Failed => answer_failed_refusal(host),
        })?;
    Ok(scan_answer(stand_ins, scrubbed, head_only, unknown))
}
