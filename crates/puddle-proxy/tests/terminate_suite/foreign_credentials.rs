// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of the warning about a credential Puddle did not add: a token the workspace's tool set
//! itself goes out unchanged, the connection record names its kind, one notice is raised for a
//! host and kind, a setting turns the warning off, and no value reaches a record, an event or a
//! log. Every token here is made up.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_proxy::{
    ForeignCredentialWarning, InjectDecision, SecretValue, StandIn, StandInOrigin, StandIns,
    TerminationSet, secret_stand_in,
};
use puddle_test_kit::with_checksum;
use puddle_types::{CollectingSink, Event, ForeignCredentialKind};
use terminate_support::h2_rig::{H2Server, Script, full, reply};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Reply, RigBuilder, TestInjector, captured_logs,
};

const ENTROPY: &str = "q1W2e3R4t5Y6u7I8o9P0a1S2d3F4g5";
const REAL_KEY: &str = "sk-live-5c1d9e0a47b2";

/// A made-up GitHub-shaped token with the checksum real ones carry, built from `ENTROPY`.
fn github_token() -> String {
    with_checksum("ghp_", ENTROPY)
}

fn ok_handler() -> Handler {
    Arc::new(|_| Reply::ok("hello from upstream"))
}

async fn upstream(pki: &Pki, name: &str) -> FakeServer {
    FakeServer::tls(pki.server_config(name, Flaw::None), ok_handler()).await
}

fn pass_through() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThrough)
}

fn git_own_credential() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThroughOwnCredential)
}

fn notices(sink: &CollectingSink) -> Vec<(String, ForeignCredentialKind)> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            Event::ForeignCredential { host, kind, .. } => Some((host, kind)),
            _ => None,
        })
        .collect()
}

/// Nothing that reaches a record, an event or a log carries `value`.
fn assert_no_value(value: &str, record: &impl std::fmt::Debug, sink: &CollectingSink) {
    assert!(!format!("{record:?}").contains(value), "the record");
    assert!(!format!("{:?}", sink.events()).contains(value), "an event");
    assert!(!captured_logs().contains(value), "a log");
}

async fn one_request(rig: &terminate_support::Rig, head: &str) {
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client.send(head.as_bytes()).await;
    assert_eq!(client.response("GET").await.status, 200);
    client.close().await;
}

#[tokio::test]
async fn a_known_token_goes_out_unchanged_and_the_record_and_one_notice_name_its_kind() {
    let pki = Pki::new();
    let server = upstream(&pki, "bound.test").await;
    let sink = Arc::new(CollectingSink::default());
    let token = github_token();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    let head = format!(
        "GET /v1/user HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    one_request(&rig, &head).await;
    one_request(&rig, &head).await;

    let seen = server.recorded();
    assert_eq!(
        seen[0].headers_named("authorization"),
        [format!("Bearer {token}")],
        "the request is allowed as it always was"
    );
    let events = rig.events(2).await;
    for event in &events {
        assert_eq!(event.foreign_credentials, ["github_token"]);
        assert!(!event.injected);
    }
    assert_eq!(
        notices(&sink),
        [("bound.test".to_owned(), ForeignCredentialKind::GithubToken)],
        "one notice for the host and kind, not one for each request"
    );
    assert_no_value(&token, &events, &sink);
    assert_no_value(ENTROPY, &events, &sink);
}

#[tokio::test]
async fn any_authorization_on_a_git_path_is_flagged_and_off_a_git_path_an_unknown_one_is_silent() {
    let pki = Pki::new();
    let server = upstream(&pki, "bound.test").await;
    let opaque = "opaque-credential-0f9e8d7c";
    let head = format!(
        "GET /org/repo/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {opaque}\r\n\r\n"
    );

    let sink = Arc::new(CollectingSink::default());
    let git = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(git_own_credential())
        .sink(&sink)
        .build();
    one_request(&git, &head).await;
    let events = git.events(1).await;
    assert_eq!(events[0].foreign_credentials, ["git_authorization"]);
    assert_eq!(
        notices(&sink),
        [(
            "bound.test".to_owned(),
            ForeignCredentialKind::GitAuthorization
        )]
    );
    assert_no_value(opaque, &events, &sink);

    let quiet_sink = Arc::new(CollectingSink::default());
    let elsewhere = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&quiet_sink)
        .build();
    one_request(&elsewhere, &head).await;
    let events = elsewhere.events(1).await;
    assert_eq!(events[0].foreign_credentials, Vec::<String>::new());
    assert_eq!(notices(&quiet_sink), []);
}

#[tokio::test]
async fn a_stand_in_the_registry_holds_is_puddles_own_and_is_not_flagged() {
    let pki = Pki::new();
    let server = upstream(&pki, "bound.test").await;
    let stand_ins = Arc::new(StandIns::new());
    let stand_in = secret_stand_in("API_KEY").unwrap();
    stand_ins
        .insert(
            StandIn::new(
                StandInOrigin::Secret,
                "API_KEY",
                &stand_in,
                SecretValue::new(REAL_KEY),
                TerminationSet::parse(["bound.test"]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(git_own_credential())
        .stand_ins(&stand_ins)
        .sink(&sink)
        .build();
    let head = format!(
        "GET /org/repo/info/refs HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {stand_in}\r\n\r\n"
    );
    one_request(&rig, &head).await;
    let events = rig.events(1).await;
    assert_eq!(events[0].foreign_credentials, Vec::<String>::new());
    assert_eq!(notices(&sink), []);
}

#[tokio::test]
async fn a_stand_in_puddle_no_longer_holds_is_flagged_as_stale() {
    let pki = Pki::new();
    let server = upstream(&pki, "bound.test").await;
    let stale = secret_stand_in("OLD_KEY").unwrap();
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(git_own_credential())
        .sink(&sink)
        .build();
    let head = format!(
        "GET /org/repo/info/refs HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {stale}\r\n\r\n"
    );
    one_request(&rig, &head).await;
    let events = rig.events(1).await;
    assert_eq!(
        events[0].foreign_credentials,
        ["stale_stand_in"],
        "a stale stand-in is named by itself, not also as an Authorization"
    );
    assert_eq!(
        notices(&sink),
        [("bound.test".to_owned(), ForeignCredentialKind::StaleStandIn)]
    );
}

#[tokio::test]
async fn with_the_warning_off_the_request_still_goes_out_and_nothing_is_flagged() {
    let pki = Pki::new();
    let server = upstream(&pki, "bound.test").await;
    let sink = Arc::new(CollectingSink::default());
    let on = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let switch = Arc::clone(&on);
    let warning = Arc::new(ForeignCredentialWarning::new(move || {
        switch.load(std::sync::atomic::Ordering::Relaxed)
    }));
    let token = github_token();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .foreign_warning(&warning)
        .sink(&sink)
        .build();
    let head = format!(
        "GET /v1/user HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {token}\r\n\r\n"
    );
    one_request(&rig, &head).await;
    assert_eq!(
        server.recorded()[0].headers_named("authorization"),
        [format!("Bearer {token}")]
    );
    assert_eq!(notices(&sink), []);

    // The setting is read when a credential is found, so turning it on applies to the next request.
    on.store(true, std::sync::atomic::Ordering::Relaxed);
    one_request(&rig, &head).await;
    let events = rig.events(2).await;
    assert_eq!(events[0].foreign_credentials, Vec::<String>::new());
    assert_eq!(events[1].foreign_credentials, ["github_token"]);
    assert_eq!(notices(&sink).len(), 1);
}

fn ok_script() -> Script {
    Arc::new(|_| reply(200, "ok"))
}

#[tokio::test]
async fn a_known_token_in_basic_credentials_on_an_http2_request_is_flagged_too() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "bound.test", ok_script()).await;
    let sink = Arc::new(CollectingSink::default());
    let token = github_token();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.h2("bound.test:443", &[b"h2", b"http/1.1"]).await;
    let basic = format!(
        "Basic {}",
        STANDARD.encode(format!("x-access-token:{token}"))
    );
    let got = client
        .request(
            "GET",
            "bound.test",
            "/v1/user",
            &[("authorization", &basic)],
            full(bytes::Bytes::new()),
        )
        .await;
    assert_eq!(got.status, 200);
    client.close().await;
    let events = rig.events(1).await;
    assert_eq!(events[0].foreign_credentials, ["github_token"]);
    assert_eq!(
        server.recorded()[0].header("authorization"),
        Some(basic.as_str())
    );
    assert_eq!(
        notices(&sink),
        [("bound.test".to_owned(), ForeignCredentialKind::GithubToken)]
    );
    assert_no_value(&token, &events, &sink);
}
