// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of the stand-ins a secret places in a query parameter or in the path: the real value
//! reaches the secret's own host in that one place, over HTTP/1.1 and HTTP/2; a stand-in toward
//! another host or in another place goes out unchanged and is named; neither the real value nor
//! the stand-in reaches the record or a log.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::Arc;

use puddle_proxy::{
    InjectDecision, Placement, SecretValue, StandIn, StandInOrigin, StandIns, TerminationSet,
    secret_stand_in,
};
use puddle_types::ConnectionEvent;
use terminate_support::h2_rig::{H2Server, Script, full, reply};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Reply, RigBuilder, TestInjector, captured_logs,
};

/// What the real server must get, and nothing else must ever see.
const REAL_KEY: &str = "owm-live-5c1d9e0a47b2";
/// A Telegram-style bot token: the `:` stays as it is in a path.
const REAL_BOT: &str = "123456789:AAlive-8f3b2a91c4d7e60";

struct Secrets {
    registry: Arc<StandIns>,
    /// The stand-in of `OWM_KEY`, placed in the query parameter `appid`.
    key: String,
    /// The stand-in of `BOT_TOKEN`, placed in the path.
    bot: String,
}

fn placed(name: &str, real: &str, placement: Placement) -> (String, StandIn) {
    let stand_in = secret_stand_in(name).unwrap();
    let entry = StandIn::new(
        StandInOrigin::Secret,
        name,
        &stand_in,
        SecretValue::new(real),
        TerminationSet::parse(["bound.test"]).unwrap(),
    )
    .unwrap()
    .placed(placement);
    (stand_in, entry)
}

/// `OWM_KEY` in `appid` and `BOT_TOKEN` in the path, both for `bound.test`.
fn secrets() -> Secrets {
    let registry = Arc::new(StandIns::new());
    let (key, entry) = placed(
        "OWM_KEY",
        REAL_KEY,
        Placement::Query(puddle_types::QueryName::new("appid").unwrap()),
    );
    registry.insert(entry).unwrap();
    let (bot, entry) = placed("BOT_TOKEN", REAL_BOT, Placement::Path);
    registry.insert(entry).unwrap();
    Secrets { registry, key, bot }
}

fn pass_through() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThrough)
}

fn ok_handler() -> Handler {
    Arc::new(|_| Reply::ok("hello from upstream"))
}

fn ok_script() -> Script {
    Arc::new(|_| reply(200, "ok"))
}

/// Neither a real value nor a stand-in is in the record or any log line.
fn assert_nothing_leaked(event: &ConnectionEvent, secrets: &Secrets) {
    let audit = format!("{event:?}");
    // The failure message names the value, never prints it.
    let values = [REAL_KEY, REAL_BOT, &secrets.key, &secrets.bot];
    let names = ["real key", "real bot token", "key stand-in", "bot stand-in"];
    for (name, text) in names.into_iter().zip(values) {
        assert!(!audit.contains(text), "the {name} is in the record");
    }
    let logs = captured_logs();
    assert!(
        !logs.contains(REAL_KEY) && !logs.contains(REAL_BOT),
        "a real value reached a log"
    );
}

#[tokio::test]
async fn query_and_path_stand_ins_are_swapped_toward_their_host_and_the_record_shows_labels() {
    let pki = Pki::new();
    let server = FakeServer::tls(pki.server_config("bound.test", Flaw::None), ok_handler()).await;
    let secrets = secrets();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    let (key, bot) = (&secrets.key, &secrets.bot);
    client
        .send(
            format!("GET /bot{bot}/getMe?lat=1&appid={key} HTTP/1.1\r\nHost: bound.test\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let response = client.response("GET").await;
    assert_eq!(response.status, 200);
    client.close().await;

    assert_eq!(
        server.recorded()[0].target,
        format!("/bot{REAL_BOT}/getMe?lat=1&appid={REAL_KEY}")
    );
    let event = &rig.events(1).await[0];
    assert!(event.injected, "a swap counts as a credential added");
    assert_eq!(event.unswapped_stand_ins, Vec::<String>::new());
    assert_eq!(
        event.http.as_ref().unwrap().path(),
        "/bot[secret:BOT_TOKEN]/getMe"
    );
    assert_nothing_leaked(event, &secrets);
}

#[tokio::test]
async fn hostile_query_and_path_stand_ins_toward_another_decrypted_host_go_out_unchanged() {
    let pki = Pki::new();
    let home = FakeServer::tls(pki.server_config("bound.test", Flaw::None), ok_handler()).await;
    let elsewhere = FakeServer::tls(
        pki.server_config("elsewhere.test", Flaw::None),
        ok_handler(),
    )
    .await;
    let secrets = secrets();
    let rig = RigBuilder::new(&pki)
        .bound(vec!["bound.test", "elsewhere.test"])
        .allow(vec!["bound.test", "elsewhere.test"])
        .name("bound.test", home.addr)
        .name("elsewhere.test", elsewhere.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("elsewhere.test:443", None).await.unwrap();
    let target = format!("/bot{}/getMe?appid={}", secrets.bot, secrets.key);
    client
        .send(format!("GET {target} HTTP/1.1\r\nHost: elsewhere.test\r\n\r\n").as_bytes())
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    client.close().await;

    assert_eq!(elsewhere.recorded()[0].target, target, "worthless there");
    assert!(home.recorded().is_empty());
    let event = &rig.events(1).await[0];
    assert!(!event.injected);
    assert_eq!(
        event.unswapped_stand_ins,
        ["stand-in:secret:OWM_KEY", "stand-in:secret:BOT_TOKEN"]
    );
    assert_eq!(
        event.http.as_ref().unwrap().path(),
        "/bot[secret:BOT_TOKEN]/getMe"
    );
    assert_nothing_leaked(event, &secrets);
}

#[tokio::test]
async fn hostile_query_and_path_stand_ins_in_another_place_toward_their_host_go_out_unchanged() {
    let pki = Pki::new();
    let server = FakeServer::tls(pki.server_config("bound.test", Flaw::None), ok_handler()).await;
    let secrets = secrets();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    let (key, bot) = (&secrets.key, &secrets.bot);
    // The query stand-in in a header, in the path and under another name; the path stand-in in
    // the query and in a header; the query parameter given twice.
    let target = format!("/{key}/x?token={bot}&key={key}&appid={key}&appid={key}");
    client
        .send(
            format!("GET {target} HTTP/1.1\r\nHost: bound.test\r\nX-Api-Key: {key}\r\nAuthorization: Bearer {bot}\r\n\r\n")
                .as_bytes(),
        )
        .await;
    assert_eq!(client.response("GET").await.status, 200);
    client.close().await;

    let seen = &server.recorded()[0];
    assert_eq!(seen.target, target, "the target is sent as it came");
    assert_eq!(seen.headers_named("x-api-key"), [key.as_str()]);
    assert_eq!(
        seen.headers_named("authorization"),
        [format!("Bearer {bot}")]
    );
    let event = &rig.events(1).await[0];
    assert!(!event.injected, "nothing was swapped");
    assert_eq!(
        event.unswapped_stand_ins,
        ["stand-in:secret:OWM_KEY", "stand-in:secret:BOT_TOKEN"]
    );
    assert_eq!(event.http.as_ref().unwrap().path(), "/[secret:OWM_KEY]/x");
    assert_nothing_leaked(event, &secrets);
}

#[tokio::test]
async fn an_http2_request_gets_its_query_and_path_stand_ins_swapped_toward_either_server() {
    let pki = Pki::new();
    let h2 = H2Server::recording(&pki, "bound.test", ok_script()).await;
    let h1 = FakeServer::tls(pki.server_config("bound.test", Flaw::None), ok_handler()).await;
    let secrets = secrets();
    let (key, bot) = (&secrets.key, &secrets.bot);
    let path = format!("/bot{bot}/getMe?appid={key}");
    let expected = format!("/bot{REAL_BOT}/getMe?appid={REAL_KEY}");
    for (addr, offer) in [
        (h2.addr, &[b"h2".as_slice(), b"http/1.1".as_slice()][..]),
        (h1.addr, &[b"h2".as_slice()][..]),
    ] {
        let rig = RigBuilder::new(&pki)
            .name("bound.test", addr)
            .injector(pass_through())
            .stand_ins(&secrets.registry)
            .build();
        let mut guest = rig.guest();
        let mut client = guest.h2("bound.test:443", offer).await;
        let got = client
            .request("GET", "bound.test", &path, &[], full(bytes::Bytes::new()))
            .await;
        assert_eq!(got.status, 200);
        client.close().await;
        let event = &rig.events(1).await[0];
        assert!(event.injected);
        assert_eq!(event.unswapped_stand_ins, Vec::<String>::new());
        assert_eq!(
            event.http.as_ref().unwrap().path(),
            "/bot[secret:BOT_TOKEN]/getMe"
        );
        assert_nothing_leaked(event, &secrets);
    }
    assert_eq!(h2.recorded()[0].path, expected);
    assert_eq!(h1.recorded()[0].target, expected);
}

#[tokio::test]
async fn hostile_an_http2_stand_in_in_another_place_or_toward_another_host_goes_out_unchanged() {
    let pki = Pki::new();
    let home = H2Server::recording(&pki, "bound.test", ok_script()).await;
    let elsewhere = H2Server::recording(&pki, "elsewhere.test", ok_script()).await;
    let secrets = secrets();
    let rig = RigBuilder::new(&pki)
        .bound(vec!["bound.test", "elsewhere.test"])
        .allow(vec!["bound.test", "elsewhere.test"])
        .name("bound.test", home.addr)
        .name("elsewhere.test", elsewhere.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let (key, bot) = (&secrets.key, &secrets.bot);
    let away = format!("/bot{bot}/getMe?appid={key}");
    let misplaced = format!("/{key}/x?token={bot}");
    for (host, path) in [("elsewhere.test", &away), ("bound.test", &misplaced)] {
        let mut guest = rig.guest();
        let mut client = guest.h2(&format!("{host}:443"), &[b"h2"]).await;
        let got = client
            .request(
                "GET",
                host,
                path,
                &[("x-api-key", key.as_str())],
                full(bytes::Bytes::new()),
            )
            .await;
        assert_eq!(got.status, 200);
        client.close().await;
    }
    assert_eq!(elsewhere.recorded()[0].path, away);
    let seen = &home.recorded()[0];
    assert_eq!(seen.path, misplaced);
    assert_eq!(seen.headers_named("x-api-key"), [key.as_str()]);
    for event in rig.events(2).await {
        assert!(!event.injected, "{event:?}");
        assert_eq!(
            event.unswapped_stand_ins,
            ["stand-in:secret:OWM_KEY", "stand-in:secret:BOT_TOKEN"]
        );
        assert_nothing_leaked(&event, &secrets);
    }
}

#[tokio::test]
async fn hostile_a_redirect_that_echoes_the_url_hands_the_guest_the_stand_in_not_the_real_value() {
    let pki = Pki::new();
    // A server that adds a slash and repeats the URL it got, as many do.
    let echo: Handler = Arc::new(|request| {
        let to = format!("https://bound.test{}/", request.target);
        Reply::status(
            301,
            &format!("location: {to}\r\nlink: <{to}>; rel=\"canonical\"\r\n"),
            "",
        )
    });
    let server = FakeServer::tls(pki.server_config("bound.test", Flaw::None), echo).await;
    let h2 = H2Server::recording(
        &pki,
        "bound.test",
        Arc::new(|seen| {
            let mut answer = reply(301, "");
            answer.headers_mut().insert(
                "location",
                format!("https://bound.test{}/", seen.path).parse().unwrap(),
            );
            answer
        }),
    )
    .await;
    let secrets = secrets();
    let (key, bot) = (&secrets.key, &secrets.bot);
    let path = format!("/bot{bot}/getMe?appid={key}");
    let echoed = format!("https://bound.test{path}/");

    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(format!("GET {path} HTTP/1.1\r\nHost: bound.test\r\n\r\n").as_bytes())
        .await;
    let response = client.response("GET").await;
    client.close().await;
    assert_eq!(
        server.recorded()[0].target,
        format!("/bot{REAL_BOT}/getMe?appid={REAL_KEY}")
    );
    assert_eq!(response.status, 301);
    assert_eq!(response.header("location"), Some(echoed.as_str()));
    assert_eq!(
        response.header("link"),
        Some(format!("<{echoed}>; rel=\"canonical\"").as_str())
    );
    assert_nothing_leaked(&rig.events(1).await[0], &secrets);

    let rig = RigBuilder::new(&pki)
        .name("bound.test", h2.addr)
        .injector(pass_through())
        .stand_ins(&secrets.registry)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
    let got = client
        .request("GET", "bound.test", &path, &[], full(bytes::Bytes::new()))
        .await;
    client.close().await;
    assert_eq!(got.status, 301);
    assert_eq!(got.header("location"), Some(echoed.as_str()));
    assert_nothing_leaked(&rig.events(1).await[0], &secrets);
}
