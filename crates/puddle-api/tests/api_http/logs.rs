// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /api/logs/bundle` over real HTTP: the download's headers.

use crate::common::start;

#[tokio::test]
async fn a_log_bundle_is_one_named_download_that_is_never_cached() {
    let api = start().await;
    let reply = api.send("POST", "/api/logs/bundle", None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("content-type"), Some("application/gzip"));
    assert_eq!(
        reply.header("content-disposition"),
        // The test clock starts at 2023-11-14T22:13:20Z, 1_700_000_000 seconds after 1970.
        Some("attachment; filename=\"puddle-logs-1700000000.tar.gz\"")
    );
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    let get = api.get("/api/logs/bundle").await;
    assert_ne!(get.status, 200, "a GET must not start a bundle");
    api.running.shutdown().await;
}
