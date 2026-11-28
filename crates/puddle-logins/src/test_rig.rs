// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one rig the tests of this crate share: a [`Logins`] on an in-memory store with the notices
//! it was told, the driver that gives it an answer the way the proxy does, and the JWT builders.
//! No token here is a real one; every value is built by the test.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use puddle_proxy::{AnswerHead, Exchange, ExchangeRewriter, Rewrite, StandIns, Want};
use puddle_secrets::MemoryStore;
use puddle_types::{Host, ReasonCode, WorkspaceId};
use serde_json::{Value, json};

use crate::vault::Vault;
use crate::{LoginNotices, Logins, Marker, Problem, Profile};

/// What the user was told, in order, as (profile id, problem).
#[derive(Default)]
pub(crate) struct Notices(Mutex<Vec<(String, Problem)>>);

impl LoginNotices for Notices {
    fn problem(&self, _: &WorkspaceId, profile: &Profile, problem: Problem) {
        self.0
            .lock()
            .unwrap()
            .push((profile.id.to_owned(), problem));
    }
}

impl Notices {
    pub(crate) fn seen(&self) -> Vec<(String, Problem)> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn problems(&self) -> Vec<Problem> {
        self.seen()
            .into_iter()
            .map(|(_, problem)| problem)
            .collect()
    }
}

pub(crate) struct Rig {
    pub(crate) logins: Logins,
    pub(crate) registry: Arc<StandIns>,
    pub(crate) store: Arc<MemoryStore>,
    pub(crate) notices: Arc<Notices>,
}

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::new("alpha").unwrap()
}

/// A rig for workspace `alpha` over `store`.
pub(crate) fn rig_with(
    profiles: &'static [Profile],
    store: &Arc<MemoryStore>,
    enabled: bool,
    marker: Marker,
) -> Rig {
    let registry = Arc::new(StandIns::new());
    let notices = Arc::new(Notices::default());
    let logins = Logins::new(
        workspace(),
        enabled,
        profiles,
        Arc::clone(&registry),
        store.clone(),
        notices.clone(),
        marker,
    );
    Rig {
        logins,
        registry,
        store: Arc::clone(store),
        notices,
    }
}

/// A rig with capture on, an empty store and no marker.
pub(crate) fn rig_for(profiles: &'static [Profile]) -> Rig {
    rig_with(
        profiles,
        &Arc::new(MemoryStore::new()),
        true,
        Marker::none(),
    )
}

pub(crate) fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

pub(crate) fn vault(rig: &Rig) -> Vault {
    Vault::new(rig.store.clone(), &workspace())
}

/// What the proxy does with an answer: asks the rewriter, shows it the head, gives it the body.
/// The result is what the guest would get: the body as sent (`Keep`), a replacement, or a refusal.
pub(crate) async fn rewritten(
    exchange: Option<Exchange>,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) -> Rewrite {
    let mut exchange = exchange.expect("the request is an exchange");
    let answer = exchange.answer_mut().expect("an exchange reads the answer");
    let head = AnswerHead {
        status,
        content_type,
        content_encoding: None,
        content_length: Some(body.len() as u64),
    };
    match answer.wants(&head) {
        Want::Read => answer.rewrite(&head, body).await,
        Want::Pass => Rewrite::Keep,
        Want::Refuse(refusal) => Rewrite::Refuse(refusal),
    }
}

/// The replacement body the guest gets, or `None` when the answer reaches it as sent. A refusal
/// fails the test: it is not "the answer went through".
pub(crate) async fn answered_bytes(
    exchange: Option<Exchange>,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) -> Option<Bytes> {
    match rewritten(exchange, status, content_type, body).await {
        Rewrite::Replace(bytes) => Some(bytes),
        Rewrite::Keep => None,
        Rewrite::Refuse(refusal) => panic!("the answer was refused: {refusal:?}"),
    }
}

/// Fails the test unless the guest gets the refusal for a sign-in that was not kept, and not the
/// real answer (`Keep`) or a replacement.
#[track_caller]
pub(crate) fn assert_refused(rewrite: &Rewrite) {
    let Rewrite::Refuse(refusal) = rewrite else {
        panic!("the real answer can reach the guest: {rewrite:?}");
    };
    assert_eq!(refusal.status(), 502);
    assert_eq!(refusal.code(), ReasonCode::LoginNotKept);
}

/// As [`answered_bytes`] for a sign-in that must not be kept: the guest gets the refusal.
pub(crate) async fn refused(
    exchange: Option<Exchange>,
    status: u16,
    content_type: Option<&str>,
    body: &[u8],
) {
    assert_refused(&rewritten(exchange, status, content_type, body).await);
}

/// The JSON the guest gets in place of a `200` JSON answer `body` to `method host path`, or
/// `None` when it gets anything else.
pub(crate) async fn answer_json(
    rig: &Rig,
    host_name: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Option<Value> {
    let bytes = serde_json::to_vec(body).unwrap();
    let exchange = rig.logins.begin(&host(host_name), method, path);
    match rewritten(
        exchange,
        200,
        Some("application/json; charset=utf-8"),
        &bytes,
    )
    .await
    {
        Rewrite::Replace(out) => Some(serde_json::from_slice(&out).unwrap()),
        _ => None,
    }
}

/// As [`answer_json`] for an answer that must be captured.
pub(crate) async fn answered(
    rig: &Rig,
    host_name: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> Value {
    answer_json(rig, host_name, method, path, body)
        .await
        .expect("captured")
}

/// A JWT with the header `{"alg":"RS256","typ":"JWT"}`, the claims text `claims` and `signature`.
pub(crate) fn jwt_signed(claims: &str, signature: &str) -> String {
    format!(
        "{}.{}.{signature}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims),
    )
}

/// A JWT of `signature_len` signature characters and the claims `claims`.
pub(crate) fn jwt(claims: &Value, signature_len: usize) -> String {
    jwt_signed(&claims.to_string(), &"S".repeat(signature_len))
}

/// The part of a JWT at `index` (0 header, 1 claims) as JSON.
pub(crate) fn part(token: &str, index: usize) -> Value {
    let segment = token.split('.').nth(index).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(segment).unwrap()).unwrap()
}

pub(crate) fn claims_of(token: &str) -> Value {
    part(token, 1)
}

/// A JWT shaped like an Entra access token: the claims a tool reads, a padding claim that brings it
/// to about `size` characters (real ones carry roles, groups and device ids), and a signature of
/// the length a real one has, made of `signature`.
pub(crate) fn sized_jwt(aud: &str, exp: u64, size: usize, signature: char) -> String {
    let signature = signature.to_string().repeat(342);
    let claims = |pad: &str| {
        json!({"aud": aud, "tid": "tenant-1", "upn": "me@example.invalid", "exp": exp, "pad": pad})
            .to_string()
    };
    let used = jwt_signed(&claims(""), &signature).len();
    // The payload's base64 is 4/3 of its text.
    let pad = "p".repeat(size.saturating_sub(used) * 3 / 4);
    jwt_signed(&claims(&pad), &signature)
}
