// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeMap;
use std::sync::Arc;

use puddle_secrets::Secret;
use puddle_store::ManualClock;

use super::*;
use crate::api::Authorization;
use crate::fake::FakeApi;
use crate::limits;

fn exchange<'a>(api: &'a FakeApi, clock: &'a ManualClock, remedy: Remedy) -> Exchange<'a> {
    Exchange {
        api,
        clock,
        strikes: 0,
        who: "GitHub",
        remedy,
        judge: limits::github,
        missing: "listing".to_owned(),
    }
}

fn request() -> ApiRequest {
    ApiRequest {
        host: "api.github.com".to_owned(),
        path: "/user".to_owned(),
        accept: "application/json",
        headers: &[],
        authorization: Authorization::Bearer(Arc::new(Secret::new("CANARY-t".to_owned()))),
        if_none_match: None,
    }
}

fn reply(status: u16, headers: &[(&str, &str)]) -> ApiReply {
    ApiReply {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect::<BTreeMap<_, _>>(),
        body: br#"{"message":"No."}"#.to_vec(),
    }
}

async fn problem(status: u16, headers: &[(&str, &str)], remedy: Remedy) -> Problem {
    let api = FakeApi::new();
    api.reply("api.github.com", "/user", reply(status, headers));
    let clock = ManualClock::new(1_000);
    exchange(&api, &clock, remedy)
        .get(request())
        .await
        .unwrap_err()
}

#[tokio::test]
async fn a_usable_answer_comes_back_as_it_is() {
    let api = FakeApi::new();
    api.reply("api.github.com", "/user", reply(200, &[]));
    let clock = ManualClock::new(1_000);
    let got = exchange(&api, &clock, Remedy::SignIn)
        .get(request())
        .await
        .unwrap();
    assert_eq!(got.status, 200);
}

#[tokio::test]
async fn a_rejected_token_says_how_to_get_a_new_one_by_kind_of_source() {
    let signed_in = problem(401, &[], Remedy::SignIn).await;
    assert_eq!(signed_in.kind, ProblemKind::TokenRejected);
    assert!(signed_in.needs_sign_in);
    assert!(signed_in.message.ends_with("sign in again"));
    let pasted = problem(401, &[], Remedy::NewToken).await;
    assert!(!pasted.needs_sign_in);
    assert!(pasted.message.ends_with("paste a new token"));
}

#[tokio::test]
async fn a_limit_carries_when_to_ask_again_and_which_kind_it_is() {
    let secondary = problem(403, &[("retry-after", "60")], Remedy::SignIn).await;
    assert_eq!(secondary.kind, ProblemKind::RateLimited);
    assert_eq!(secondary.retry_at, Some(61_000));
    assert!(secondary.message.contains("limiting how fast"));
    let primary = problem(
        429,
        &[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "100")],
        Remedy::SignIn,
    )
    .await;
    assert_eq!(primary.retry_at, Some(100_000));
    assert!(primary.message.contains("rate limit is used up"));
}

#[tokio::test]
async fn refusals_and_unknown_statuses_say_what_the_host_said() {
    let forbidden = problem(403, &[], Remedy::SignIn).await;
    assert_eq!(forbidden.kind, ProblemKind::Forbidden);
    assert_eq!(forbidden.message, "GitHub refused the request: No.");
    let missing = problem(404, &[], Remedy::SignIn).await;
    assert_eq!(missing.kind, ProblemKind::NotFound);
    assert_eq!(
        missing.message,
        "GitHub has no listing that this token can see"
    );
    let odd = problem(418, &[], Remedy::SignIn).await;
    assert_eq!(odd.kind, ProblemKind::BadAnswer);
    assert!(odd.message.contains("answered 418"));
}

#[test]
fn a_refusal_with_no_words_is_still_a_sentence() {
    let api = FakeApi::new();
    let clock = ManualClock::new(0);
    let p = exchange(&api, &clock, Remedy::SignIn).problem(Verdict::Forbidden(None));
    assert_eq!(
        p.message,
        "GitHub refused the request (403) and gave no reason"
    );
    let ok = exchange(&api, &clock, Remedy::SignIn).problem(Verdict::Ok);
    assert_eq!(ok.kind, ProblemKind::BadAnswer);
}

#[tokio::test]
async fn a_transport_failure_names_the_host_and_the_cause() {
    let cases = [
        (
            TransportError::Unreachable("no route".to_owned()),
            ProblemKind::Unreachable,
            "the host could not be reached: api.github.com: no route",
        ),
        (
            TransportError::Tls("unknown issuer".to_owned()),
            ProblemKind::Unreachable,
            "the connection to api.github.com is not trusted: unknown issuer; if the company network re-signs HTTPS, its root certificate must be trusted by this computer",
        ),
        (
            TransportError::Timeout(30),
            ProblemKind::Unreachable,
            "the host could not be reached: api.github.com: no answer within 30 seconds",
        ),
        (
            TransportError::TooLarge,
            ProblemKind::BadAnswer,
            "the host answered something Puddle does not understand: the answer is larger than 16 MiB",
        ),
        (
            TransportError::BadToken,
            ProblemKind::SourceUnavailable,
            "the token has a character an HTTP header cannot carry",
        ),
    ];
    for (err, kind, message) in cases {
        let api = FakeApi::new();
        api.fail("api.github.com", "/user", err);
        let clock = ManualClock::new(0);
        let p = exchange(&api, &clock, Remedy::SignIn)
            .get(request())
            .await
            .unwrap_err();
        assert_eq!((p.kind, p.message.as_str()), (kind, message));
    }
}
