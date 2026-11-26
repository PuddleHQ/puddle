// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests that the audit record of a Git smart-HTTP connection says whether it pushes or
//! fetches. The first request of both is `GET /info/refs`, and only the query tells them apart.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::sync::Arc;

use puddle_proxy::InjectDecision;
use puddle_types::{GitOperation, HttpRequestLine};
use terminate_support::{FakeServer, Flaw, Pki, Reply, RigBuilder, TestInjector};

/// The line the audit keeps for the first request of a terminated connection that sent `request`.
async fn recorded_line(request: &str) -> HttpRequestLine {
    let pki = Pki::new();
    let server = FakeServer::tls(
        pki.server_config("bound.test", Flaw::None),
        Arc::new(|_| Reply::ok("refs")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(TestInjector::new(|_| InjectDecision::PassThrough))
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client.send(request.as_bytes()).await;
    assert_eq!(client.response("GET").await.status, 200);
    client.close().await;
    rig.events(1)
        .await
        .into_iter()
        .next()
        .unwrap()
        .http
        .unwrap()
}

#[tokio::test]
async fn a_push_and_a_fetch_are_told_apart_in_the_record() {
    let push = recorded_line(
        "GET /acme/web.git/info/refs?service=git-receive-pack HTTP/1.1\r\nHost: bound.test\r\n\r\n",
    )
    .await;
    assert_eq!(push.path(), "/acme/web.git/info/refs");
    assert_eq!(push.git_operation(), Some(GitOperation::Push));

    let fetch = recorded_line(
        "GET /acme/web.git/info/refs?service=git-upload-pack HTTP/1.1\r\nHost: bound.test\r\n\r\n",
    )
    .await;
    assert_eq!(fetch.path(), "/acme/web.git/info/refs");
    assert_eq!(fetch.git_operation(), Some(GitOperation::Fetch));

    let other = recorded_line("GET /acme/web HTTP/1.1\r\nHost: bound.test\r\n\r\n").await;
    assert_eq!(other.git_operation(), None);
}
