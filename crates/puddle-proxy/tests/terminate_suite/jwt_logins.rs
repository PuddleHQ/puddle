// SPDX-License-Identifier: GPL-3.0-or-later
//! A login whose access token is a JWT, through the real proxy: the tool reads the claims of the
//! stand-in it is given, and the stand-ins it was given in the last few exchanges are each swapped for
//! the real token of their own exchange toward the service's API host. A fake server stands in for the service.
use crate::terminate_support;

use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use puddle_logins::{
    Audience, Endpoint, Field, Keep, LoginNotices, Logins, Method, Problem, Profile, Role, Scope,
    StandInKind,
};
use puddle_proxy::{InjectDecision, StandIns};
use puddle_secrets::{MemoryStore, SecretStore};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Recorded, Reply, RigBuilder, TestInjector,
};

const TOKEN_HOST: &str = "login.jwt.test";
const API_HOST: &str = "api.jwt.test";

static PROFILE: [Profile; 1] = [Profile {
    id: "jwt",
    name: "Jwt",
    endpoints: &[Endpoint {
        host: TOKEN_HOST,
        method: Method::Post,
        path: "/token",
        request_fields: &[],
        fields: &[Field {
            path: "access_token",
            role: Role::Access,
            resource: None,
            prefixes: &[],
            keep_last: 0,
            stand_in: StandInKind::Jwt,
            only_prefixed: false,
            scope: Scope::Api,
            audience: Audience::Any,
            keep: Keep::Stored,
        }],
    }],
    api_hosts: &[API_HOST],
}];

struct Quiet(Mutex<Vec<Problem>>);

impl LoginNotices for Quiet {
    fn problem(&self, _: &WorkspaceId, _: &Profile, problem: Problem) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(problem);
    }
}

/// A JWT whose claims say who it is for and when it expires, and a signature of `fill`.
fn jwt(exp: u64, fill: char) -> String {
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(json!({"sub": "me", "exp": exp}).to_string()),
        fill.to_string().repeat(86),
    )
}

fn reply(access: &str) -> Reply {
    let body = json!({"access_token": access, "token_type": "Bearer"}).to_string();
    Reply::raw(format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    ))
}

fn request(method: &str, host: &str, path: &str, extra: &str) -> Vec<u8> {
    format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Content-Length: 0\r\n\r\n")
        .into_bytes()
}

#[tokio::test]
async fn every_jwt_stand_in_given_so_far_reaches_the_api_as_the_real_token_of_its_own_exchange() {
    let pki = Pki::new();
    let reals = [
        jwt(1_000_000_001, 'A'),
        jwt(1_000_003_601, 'B'),
        jwt(1_000_007_201, 'C'),
    ];
    let served = Arc::new(Mutex::new(0_usize));
    let handler: Handler = {
        let (reals, served) = (reals.clone(), Arc::clone(&served));
        Arc::new(move |_: &Recorded| {
            let mut n = served.lock().unwrap();
            let real = &reals[(*n).min(reals.len() - 1)];
            *n += 1;
            reply(real)
        })
    };
    let token = FakeServer::tls(pki.server_config(TOKEN_HOST, Flaw::None), handler).await;
    let api = FakeServer::tls(
        pki.server_config(API_HOST, Flaw::None),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await;
    let registry = Arc::new(StandIns::new());
    let notices = Arc::new(Quiet(Mutex::new(Vec::new())));
    let logins = Logins::new(
        WorkspaceId::new("box").unwrap(),
        true,
        &PROFILE,
        Arc::clone(&registry),
        Arc::new(MemoryStore::new()) as Arc<dyn SecretStore>,
        Arc::clone(&notices) as Arc<dyn LoginNotices>,
        puddle_logins::Marker::none(),
    );
    let rig = RigBuilder::new(&pki)
        .bound(vec![TOKEN_HOST, API_HOST])
        .allow(vec![TOKEN_HOST, API_HOST])
        .name(TOKEN_HOST, token.addr)
        .name(API_HOST, api.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .stand_ins(&registry)
        .exchanges(Arc::new(logins))
        .build();
    let mut guest = rig.guest();

    let mut given = Vec::new();
    for _ in 0..3 {
        let mut client = guest.tls(&format!("{TOKEN_HOST}:443"), None).await.unwrap();
        client
            .send(&request("POST", TOKEN_HOST, "/token", ""))
            .await;
        let response = client.response("POST").await;
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        given.push(body["access_token"].as_str().unwrap().to_owned());
        client.close().await;
    }
    for (stand_in, real) in given.iter().zip(&reals) {
        assert_ne!(stand_in, real);
        assert_eq!(stand_in.len(), real.len());
        // A tool that reads the claims reads the real ones.
        let claims = stand_in.split('.').nth(1).unwrap();
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
        assert_eq!(claims["sub"], "me");
        assert!(!stand_in.contains(real.rsplit('.').next().unwrap()));
    }
    assert_eq!(registry.len(), 3);

    // An older stand-in, used by a process that has not stored the new one, still works.
    let mut client = guest.tls(&format!("{API_HOST}:443"), None).await.unwrap();
    for (stand_in, real) in given.iter().zip(&reals) {
        client
            .send(&request(
                "GET",
                API_HOST,
                "/me",
                &format!("Authorization: Bearer {stand_in}\r\n"),
            ))
            .await;
        assert_eq!(client.response("GET").await.status, 200);
        let seen = api.recorded();
        assert_eq!(
            seen.last().unwrap().header("authorization"),
            Some(format!("Bearer {real}").as_str()),
            "{stand_in}"
        );
    }
    assert!(notices.0.lock().unwrap().is_empty());
}
