// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_secrets::Secret;

use super::*;
use crate::api::Authorization;

fn request(path: &str) -> ApiRequest {
    ApiRequest {
        host: "h.test".to_owned(),
        path: path.to_owned(),
        accept: "application/json",
        headers: &[],
        authorization: Authorization::Bearer(Arc::new(Secret::new("CANARY-1".to_owned()))),
        if_none_match: None,
    }
}

#[tokio::test]
async fn answers_come_from_the_queue_and_the_last_one_repeats() {
    let fake = FakeApi::new();
    fake.reply("h.test", "/a", ApiReply::new(200, "one"));
    fake.reply("h.test", "/a", ApiReply::new(200, "two"));
    fake.fail("h.test", "/b", TransportError::Timeout(5));
    fake.reply("h.test", "/gone", ApiReply::new(200, "x"));
    fake.clear("h.test", "/gone");
    let body = |r: Result<ApiReply, TransportError>| r.unwrap().body;
    assert_eq!(body(fake.get(request("/a")).await), b"one");
    assert_eq!(body(fake.get(request("/a")).await), b"two");
    assert_eq!(body(fake.get(request("/a")).await), b"two");
    assert_eq!(
        fake.get(request("/b")).await.unwrap_err(),
        TransportError::Timeout(5)
    );
    assert!(matches!(
        fake.get(request("/c")).await,
        Err(TransportError::Unreachable(_))
    ));
    assert!(matches!(
        fake.get(request("/gone")).await,
        Err(TransportError::Unreachable(_))
    ));
    assert_eq!(fake.count(), 6);
    assert_eq!(fake.seen()[0].token, "CANARY-1");
    assert!(fake.seen()[0].bearer);
    assert!(format!("{fake:?}").starts_with("FakeApi"));
}
