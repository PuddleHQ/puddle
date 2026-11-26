// SPDX-License-Identifier: AGPL-3.0-or-later
//! One request, both terminated protocols: each scenario runs over HTTP/1.1 and over HTTP/2 through
//! one helper, and what the guest sees, what the server receives and what the record says must be
//! the same on both. The scenarios are the steps of the shared request pipeline: a rule refusal, an
//! injector refusal, the stand-in swap in a header, the target and the body, the replaced `401`,
//! the scrub of an answer, an answer in an unreadable encoding, a token Puddle does not hold, and
//! a guest's own `x-puddle-*` header.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::fmt::Write as _;
use std::sync::Arc;

use bytes::Bytes;
use http::Response;
use http_body_util::{BodyExt as _, Full};
use puddle_proxy::{
    Exchange, ExchangeRewriter, InjectDecision, InjectRefusal, RequestRule, RequestRules,
    SecretValue, StandIn, StandInOrigin, StandIns, TerminationSet, Unauthorized, secret_stand_in,
};
use puddle_test_kit::{TokenKind, token};
use puddle_types::{ConnectionDecision, Host, Methods, PathPattern, ReasonCode, RuleId};
use terminate_support::h2_rig::{H2Server, full};
use terminate_support::{
    CANARY, FakeServer, Flaw, Pki, Reply, RigBuilder, TestInjector, inject, refuse,
};

/// What the real server answers to every request, spoken in each protocol by `run`.
#[derive(Clone)]
struct Script {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
}

const REAL: &str = "sk-live-5c1d9e0a47b2";

#[derive(Clone, Copy, Debug)]
enum Wire {
    Http1,
    Http2,
}

const WIRES: [Wire; 2] = [Wire::Http1, Wire::Http2];

/// One request a guest sends, whatever the protocol.
#[derive(Clone, Default)]
struct Ask {
    method: &'static str,
    path: String,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Ask {
    fn get(path: &str) -> Self {
        Self {
            method: "GET",
            path: path.to_owned(),
            ..Self::default()
        }
    }

    fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

/// What a run leaves behind: the guest's answer, the server's view of the request and the record.
#[derive(Debug, PartialEq)]
struct Observed {
    status: u16,
    answer_headers: Vec<(String, String)>,
    answer_body: String,
    upstream: Vec<Upstream>,
    decision: ConnectionDecision,
    reason: String,
    request_rule: Option<RuleId>,
    injected: bool,
    binding_id: Option<String>,
    unswapped: Vec<String>,
    scrubbed: Vec<String>,
    answer_credentials: Vec<String>,
}

#[derive(Debug, PartialEq)]
struct Upstream {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// Asks for the body of a `POST` to be swapped in one field.
#[derive(Debug)]
struct SwapsOneField(&'static str);

impl ExchangeRewriter for SwapsOneField {
    fn begin(&self, _host: &Host, method: &str, _path: &str) -> Option<Exchange> {
        (method == "POST").then(|| Exchange::new().swapping(self.0))
    }
}

struct Setup {
    injector: Arc<TestInjector>,
    stand_ins: Option<Arc<StandIns>>,
    rules: Vec<RequestRule>,
    /// The JSON body field of a `POST` the exchange swaps stand-ins in.
    swap_field: Option<&'static str>,
    script: Script,
}

impl Setup {
    fn new(script: Script) -> Self {
        Self {
            injector: TestInjector::new(|_| InjectDecision::PassThrough),
            stand_ins: None,
            rules: Vec::new(),
            swap_field: None,
            script,
        }
    }

    fn injector(mut self, decide: fn() -> InjectDecision) -> Self {
        self.injector = TestInjector::new(move |_| decide());
        self
    }

    fn with_stand_in(mut self) -> (Self, String) {
        let stand_ins = Arc::new(StandIns::new());
        let stand_in = secret_stand_in("API_KEY").unwrap();
        stand_ins
            .insert(
                StandIn::new(
                    StandInOrigin::Secret,
                    "API_KEY",
                    &stand_in,
                    SecretValue::new(REAL),
                    TerminationSet::parse(["bound.test"]).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
        self.stand_ins = Some(stand_ins);
        (self, stand_in)
    }
}

fn pairs(headers: &[(String, String)], names: &[&str]) -> Vec<(String, String)> {
    let mut kept: Vec<_> = headers
        .iter()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            name.starts_with("x-puddle-") || names.contains(&name.as_str())
        })
        .map(|(n, v)| (n.to_ascii_lowercase(), v.clone()))
        .collect();
    kept.sort();
    kept
}

/// The real server, speaking the guest's protocol.
enum Server {
    Http1(FakeServer),
    Http2(H2Server),
}

impl Server {
    async fn start(wire: Wire, pki: &Pki, script: &Script) -> Self {
        match wire {
            Wire::Http1 => {
                let mut raw = format!("HTTP/1.1 {} Status\r\n", script.status);
                for (name, value) in &script.headers {
                    write!(raw, "{name}: {value}\r\n").unwrap();
                }
                write!(
                    raw,
                    "content-length: {}\r\n\r\n{}",
                    script.body.len(),
                    script.body
                )
                .unwrap();
                Self::Http1(
                    FakeServer::tls(
                        pki.server_config("bound.test", Flaw::None),
                        Arc::new(move |_| Reply::raw(raw.clone())),
                    )
                    .await,
                )
            }
            Wire::Http2 => {
                let script = script.clone();
                Self::Http2(
                    H2Server::recording(
                        pki,
                        "bound.test",
                        Arc::new(move |_| {
                            let headers: Vec<(&str, &str)> = script
                                .headers
                                .iter()
                                .map(|(n, v)| (*n, v.as_str()))
                                .collect();
                            answer(script.status, &headers, &script.body)
                        }),
                    )
                    .await,
                )
            }
        }
    }

    fn addr(&self) -> std::net::SocketAddr {
        match self {
            Self::Http1(server) => server.addr,
            Self::Http2(server) => server.addr,
        }
    }

    fn received(&self) -> Vec<Upstream> {
        let keep = ["authorization", "x-api-key", "cookie"];
        match self {
            Self::Http1(server) => server
                .recorded()
                .iter()
                .map(|seen| Upstream {
                    method: seen.method.clone(),
                    path: seen.target.clone(),
                    headers: pairs(&seen.headers, &keep),
                    body: String::from_utf8(seen.body.clone()).unwrap(),
                })
                .collect(),
            Self::Http2(server) => server
                .recorded()
                .iter()
                .map(|seen| Upstream {
                    method: seen.method.clone(),
                    path: seen.path.clone(),
                    headers: pairs(&seen.headers, &keep),
                    body: String::from_utf8(seen.body.clone()).unwrap(),
                })
                .collect(),
        }
    }
}

/// Sends `ask` as a guest on `wire` and returns the answer's status, headers and body.
async fn ask_over(
    wire: Wire,
    guest: &mut terminate_support::Guest,
    ask: &Ask,
) -> (u16, Vec<(String, String)>, String) {
    match wire {
        Wire::Http1 => {
            let mut client = guest.tls("bound.test:443", None).await.unwrap();
            let mut head = format!(
                "{} {} HTTP/1.1\r\nHost: bound.test\r\nContent-Length: {}\r\n",
                ask.method,
                ask.path,
                ask.body.len()
            );
            for (name, value) in &ask.headers {
                write!(head, "{name}: {value}\r\n").unwrap();
            }
            client
                .send(format!("{head}\r\n{}", ask.body).as_bytes())
                .await;
            let answer = client.response(ask.method).await;
            client.close().await;
            (answer.status, answer.headers.clone(), answer.text())
        }
        Wire::Http2 => {
            let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
            let headers: Vec<(&str, &str)> =
                ask.headers.iter().map(|(n, v)| (*n, v.as_str())).collect();
            let answer = client
                .request(
                    ask.method,
                    "bound.test",
                    &ask.path,
                    &headers,
                    full(Bytes::from(ask.body.clone())),
                )
                .await;
            client.close().await;
            (answer.status, answer.headers.clone(), answer.text())
        }
    }
}

async fn run(wire: Wire, setup: &Setup, ask: &Ask) -> Observed {
    let pki = Pki::new();
    let server = Server::start(wire, &pki, &setup.script).await;
    let mut builder = RigBuilder::new(&pki)
        .name("bound.test", server.addr())
        .injector(setup.injector.clone())
        .allow(vec!["bound.test"])
        .request_rules(RequestRules::new(setup.rules.clone()));
    if let Some(field) = setup.swap_field {
        builder = builder.exchanges(Arc::new(SwapsOneField(field)));
    }
    if let Some(stand_ins) = &setup.stand_ins {
        builder = builder.stand_ins(stand_ins);
    }
    let rig = builder.build();
    let (status, answer_headers, answer_body) = ask_over(wire, &mut rig.guest(), ask).await;
    let event = rig.events(1).await.remove(0);
    Observed {
        status,
        answer_headers: pairs(
            &answer_headers,
            &[
                "www-authenticate",
                "set-cookie",
                "location",
                "content-encoding",
            ],
        ),
        answer_body,
        upstream: server.received(),
        decision: event.decision,
        reason: event.reason.to_string(),
        request_rule: event.request_rule,
        injected: event.injected,
        binding_id: event.binding_id,
        unswapped: event.unswapped_stand_ins,
        scrubbed: event.scrubbed_stand_ins,
        answer_credentials: event.answer_credentials,
    }
}

/// Runs the scenario on both protocols and returns the one outcome they share.
async fn on_both(setup: &Setup, ask: &Ask) -> Observed {
    let h1 = run(WIRES[0], setup, ask).await;
    let h2 = run(WIRES[1], setup, ask).await;
    assert_eq!(h1, h2, "HTTP/1.1 (left) and HTTP/2 (right) disagree");
    h1
}

fn answer(
    status: u16,
    headers: &[(&str, &str)],
    body: &str,
) -> Response<terminate_support::h2_rig::ReqBody> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder
        .body(
            Full::new(Bytes::from(body.to_owned()))
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap()
}

fn says(status: u16, headers: &[(&'static str, &str)], body: &str) -> Script {
    Script {
        status,
        headers: headers.iter().map(|(n, v)| (*n, (*v).to_owned())).collect(),
        body: body.to_owned(),
    }
}

fn ok() -> Script {
    says(200, &[], "ok")
}

fn named(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
}

#[tokio::test]
async fn a_request_a_rule_refuses_never_reaches_the_server_and_is_recorded_alike() {
    let mut setup = Setup::new(ok());
    setup.rules = vec![
        RequestRule::new(
            RuleId(7),
            "bound.test",
            Methods::parse(["POST"]).unwrap(),
            PathPattern::parse("/gists").unwrap(),
        )
        .unwrap(),
    ];
    let mut ask = Ask::get("/gists");
    ask.method = "POST";
    let seen = on_both(&setup, &ask).await;
    assert_eq!(seen.status, 403);
    assert_eq!(
        named(&seen.answer_headers, "x-puddle-request-rule").as_deref(),
        Some("7")
    );
    assert_eq!(seen.upstream.len(), 0);
    assert_eq!(seen.decision, ConnectionDecision::Deny);
    assert_eq!(seen.request_rule, Some(RuleId(7)));

    let allowed = on_both(&setup, &Ask::get("/gists")).await;
    assert_eq!((allowed.status, allowed.upstream.len()), (200, 1));
}

#[tokio::test]
async fn an_injector_refusal_is_the_same_answer_and_record_on_both() {
    let setup = Setup::new(ok()).injector(|| refuse(403, ReasonCode::CredentialRejected));
    let seen = on_both(&setup, &Ask::get("/x")).await;
    assert_eq!(seen.status, 403);
    assert_eq!(
        named(&seen.answer_headers, "x-puddle-blocked").as_deref(),
        Some("credential_rejected")
    );
    assert_eq!(seen.upstream.len(), 0);
}

#[tokio::test]
async fn an_added_credential_goes_out_and_is_recorded_alike() {
    let setup = Setup::new(ok()).injector(|| inject(CANARY));
    let seen = on_both(
        &setup,
        &Ask::get("/x").header("authorization", "Bearer mine"),
    )
    .await;
    assert_eq!(seen.status, 200);
    assert_eq!(
        named(&seen.upstream[0].headers, "authorization"),
        Some(format!("Basic {CANARY}"))
    );
    assert!(seen.injected);
    assert_eq!(seen.binding_id.as_deref(), Some("binding-1"));
}

#[tokio::test]
async fn a_stand_in_in_a_header_and_the_body_is_swapped_and_one_in_the_target_is_named() {
    let (mut setup, stand_in) = Setup::new(ok()).with_stand_in();
    setup.swap_field = Some("key");
    let mut ask = Ask::get(&format!("/v1/models?key={stand_in}"))
        .header("x-api-key", stand_in.clone())
        .header("cookie", format!("k={stand_in}"))
        .header("content-type", "application/json");
    ask.method = "POST";
    ask.body = format!("{{\"key\":\"{stand_in}\"}}");
    let seen = on_both(&setup, &ask).await;
    let sent = &seen.upstream[0];
    assert_eq!(named(&sent.headers, "x-api-key").as_deref(), Some(REAL));
    assert_eq!(named(&sent.headers, "cookie"), Some(format!("k={REAL}")));
    assert_eq!(sent.body, format!("{{\"key\":\"{REAL}\"}}"));
    assert_eq!(sent.path, format!("/v1/models?key={stand_in}"));
    assert_eq!(seen.unswapped, ["stand-in:secret:API_KEY"]);
    assert!(seen.injected);
}

#[tokio::test]
async fn a_401_for_an_added_credential_becomes_the_injectors_refusal() {
    let setup = Setup::new(says(
        401,
        &[("www-authenticate", "Basic realm=\"upstream\"")],
        "no",
    ))
    .injector(|| {
        let InjectDecision::Inject(injection) = inject(CANARY) else {
            unreachable!("inject() injects");
        };
        InjectDecision::Inject(injection.on_unauthorized(Unauthorized::new(|| {
            InjectRefusal::new(
                403,
                ReasonCode::CredentialRejected,
                "the credential was rejected",
            )
        })))
    });
    let seen = on_both(&setup, &Ask::get("/x")).await;
    assert_eq!(seen.status, 403);
    assert_eq!(
        named(&seen.answer_headers, "x-puddle-blocked").as_deref(),
        Some("credential_rejected")
    );
    assert_eq!(named(&seen.answer_headers, "www-authenticate"), None);
    assert!(!seen.answer_body.contains("no\n"));

    let own = Setup::new(says(401, &[("www-authenticate", "Basic")], "no"));
    let seen = on_both(&own, &Ask::get("/x")).await;
    assert_eq!((seen.status, seen.answer_body.as_str()), (401, "no"));
}

#[tokio::test]
async fn a_real_value_in_an_answer_comes_back_as_the_stand_in() {
    let (mut setup, stand_in) = Setup::new(ok()).with_stand_in();
    setup.script = says(
        200,
        &[
            ("set-cookie", &format!("k={REAL}; Path=/")),
            ("location", &format!("https://bound.test/cb?k={REAL}")),
        ],
        &format!("{{\"echo\":\"{REAL}\"}}"),
    );
    let seen = on_both(&setup, &Ask::get("/debug")).await;
    assert_eq!(seen.answer_body, format!("{{\"echo\":\"{stand_in}\"}}"));
    assert_eq!(
        named(&seen.answer_headers, "set-cookie"),
        Some(format!("k={stand_in}; Path=/"))
    );
    assert_eq!(
        named(&seen.answer_headers, "location"),
        Some(format!("https://bound.test/cb?k={stand_in}"))
    );
    assert_eq!(seen.scrubbed, ["stand-in:secret:API_KEY"]);
}

#[tokio::test]
async fn an_answer_in_an_encoding_puddle_cannot_read_is_refused_alike() {
    let (mut setup, _) = Setup::new(ok()).with_stand_in();
    setup.script = says(200, &[("content-encoding", "compress")], "abc");
    let seen = on_both(&setup, &Ask::get("/x")).await;
    assert_eq!(
        named(&seen.answer_headers, "x-puddle-blocked").as_deref(),
        Some("answer_encoding")
    );
    assert_ne!(seen.answer_body, "abc");
}

#[tokio::test]
async fn a_token_puddle_does_not_hold_reaches_the_guest_and_its_kind_is_recorded_alike() {
    let made = token(TokenKind::GithubPersonal, "parity");
    let body = format!("{{\"access_token\":\"{made}\"}}");
    let setup = Setup::new(says(200, &[], &body));
    let seen = on_both(&setup, &Ask::get("/login")).await;
    assert_eq!(seen.answer_body, body);
    assert_eq!(seen.answer_credentials, ["github_token"]);
}

#[tokio::test]
async fn a_guests_own_x_puddle_header_is_not_forwarded_on_either_protocol() {
    let setup = Setup::new(ok());
    let seen = on_both(&setup, &Ask::get("/x").header("x-puddle-binding", "forged")).await;
    assert_eq!(seen.status, 200);
    assert_eq!(named(&seen.upstream[0].headers, "x-puddle-binding"), None);
}
