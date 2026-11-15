// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log at `PUDDLE_LOG=trace` never holds a secret. A real-looking value goes in at every place
//! a user can put one (a stored token, a stand-in the proxy swaps, a login the proxy captures),
//! the proxy carries each through a real exchange, and the log `PUDDLE_LOG=trace` would print is
//! searched for it.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::logins::{
    ACCESS_1, REFRESH_1, TOKEN_HOST, api_call, authorization_code, claude_login, request,
};
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_proxy::{
    InjectDecision, SecretValue, StandIn, StandInOrigin, StandIns, TerminationSet, secret_stand_in,
};
use puddle_secrets::{
    Fetch, HostName, MemoryStore, Secret, SecretStore, SourceSpec, Sources, StoredId, TokenScope,
    ToolPaths,
};
use puddle_test_kit::{TokenKind, token};
use terminate_support::{
    FakeServer, Flaw, Pki, Reply, RigBuilder, TestInjector, captured_logs, captured_scoped_logs,
    scoped_trace_subscriber,
};

/// A GitHub token a user pasted into Puddle, as long and as random as a real one, shaped like one
/// (built at run time, so no scanner sees it in source).
fn stored_token() -> String {
    token(TokenKind::GithubPersonal, "log canary stored token")
}

/// The places a value can hide in a log: as typed, and as the `Authorization: Basic` header or
/// the URL's `user:token` form carries it.
fn spellings(value: &str) -> Vec<String> {
    vec![
        value.to_owned(),
        STANDARD.encode(value),
        STANDARD.encode(format!("x-access-token:{value}")),
    ]
}

/// The first of `values` (in any spelling) that `logs` holds.
fn leaked(logs: &str, values: &[&str]) -> Option<String> {
    values
        .iter()
        .flat_map(|v| spellings(v))
        .find(|spelling| logs.contains(spelling.as_str()))
}

fn stored_spec() -> SourceSpec {
    SourceSpec::Stored {
        id: StoredId::new("github-main").unwrap(),
        scope: TokenScope {
            host: HostName::new("github.com").unwrap(),
            org: None,
        },
    }
}

#[tokio::test]
async fn a_stored_secret_a_stand_in_swap_and_a_login_leave_no_value_in_the_trace_log() {
    terminate_support::capture_logs();
    let pki = Pki::new();

    let stored = stored_token();
    // A stored secret, read back through the same sources the host uses.
    let store = Arc::new(MemoryStore::new());
    store
        .set(
            &StoredId::new("github-main").unwrap(),
            &Secret::new(stored.clone()),
        )
        .unwrap();
    let sources = Sources::new(ToolPaths::new(None, None), store);
    let fetched = sources.fetch(&stored_spec()).await.unwrap();
    assert_eq!(fetched.credential.secret.expose(), stored);

    // The stand-in swap: the workspace sends the stand-in, the real server gets the real value.
    let server = FakeServer::tls(
        pki.server_config("bound.test", Flaw::None),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await;
    let stand_in = secret_stand_in("GH_TOKEN").unwrap();
    let stand_ins = Arc::new(StandIns::new());
    stand_ins
        .insert(
            StandIn::new(
                StandInOrigin::Secret,
                "GH_TOKEN",
                &stand_in,
                SecretValue::new(fetched.credential.secret.expose()),
                TerminationSet::parse(["bound.test"]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(
            format!(
                "GET /user HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {stand_in}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    assert_eq!(
        server.recorded()[0].headers_named("authorization"),
        [format!("Bearer {stored}")],
        "the real value reached the server, so the swap ran"
    );
    client.close().await;

    // A login: the proxy captures the real tokens, then swaps them back on the API call.
    let login = claude_login(&pki, true, Arc::new(MemoryStore::new())).await;
    let mut guest = login.rig.guest();
    let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
    client
        .send(&request(
            "POST",
            TOKEN_HOST,
            "/v1/oauth/token",
            "Content-Type: application/json\r\n",
            &authorization_code(),
        ))
        .await;
    let given: serde_json::Value =
        serde_json::from_slice(&client.response("POST").await.body).unwrap();
    let access = given["access_token"].as_str().unwrap().to_owned();
    assert_eq!(
        api_call(&login, &access).await.as_deref(),
        Some(format!("Bearer {ACCESS_1}").as_str()),
        "the real login token reached the service, so the capture ran"
    );
    client.close().await;

    let values = [stored.as_str(), ACCESS_1, REFRESH_1];
    let scoped = captured_scoped_logs();
    // Not an empty search: the log holds Puddle's own trace and debug lines from these requests.
    assert!(scoped.contains("reading a secret"), "{scoped}");
    assert!(
        scoped.contains(" TRACE ") || scoped.contains(" DEBUG "),
        "{scoped}"
    );
    assert_eq!(
        leaked(&scoped, &values),
        None,
        "PUDDLE_LOG=trace printed a secret"
    );
    // The same search over every crate at trace, which is stricter than the user's setting.
    assert_eq!(
        leaked(&captured_logs(), &values),
        None,
        "a crate's trace output holds a secret"
    );
}

/// The canary only counts if it fails when a line prints the value.
#[test]
fn the_search_finds_a_value_a_puddle_line_prints_in_any_spelling_and_a_dependency_trace_line_is_dropped()
 {
    let stored = stored_token();
    let sink = Arc::new(Mutex::new(Vec::new()));
    tracing::subscriber::with_default(scoped_trace_subscriber(&sink), || {
        tracing::trace!(target: "puddle_proxy::terminate", token = %stored, "swapped");
        let basic = STANDARD.encode(format!("x-access-token:{ACCESS_1}"));
        tracing::debug!(target: "puddle_proxy::git", header = %basic, "injected");
        // A dependency's trace line is not printed at all, which is what the scoping is for.
        tracing::trace!(target: "hyper::proto::h1::io", bytes = %REFRESH_1, "parsed headers");
    });
    let logs =
        String::from_utf8(sink.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap();
    assert!(logs.contains(&stored), "{logs}");
    assert_eq!(leaked(&logs, &[&stored]).as_deref(), Some(stored.as_str()));
    assert!(
        leaked(&logs, &[ACCESS_1]).is_some(),
        "the Basic form is searched too"
    );
    assert_eq!(leaked(&logs, &[REFRESH_1]), None, "{logs}");
}
