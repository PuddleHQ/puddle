// SPDX-License-Identifier: AGPL-3.0-or-later
//! Serving the single-page app from the API's origin: files need no token, the API still does,
//! and the page is served with a strict CSP and no framing.

use std::collections::HashMap;
use std::sync::Arc;

use puddle_api::{ApiConfig, UiAssets, UiFile};

use crate::common::{Api, Reply, raw, start_with};

#[derive(Debug)]
struct Files(HashMap<&'static str, (&'static str, &'static str)>);

impl UiAssets for Files {
    fn get(&self, path: &str) -> Option<UiFile> {
        self.0
            .get(path)
            .map(|(body, kind)| UiFile::new(body.as_bytes(), *kind))
    }
}

fn config() -> ApiConfig {
    let mut config = ApiConfig::default();
    config.ui = Some(Arc::new(Files(HashMap::from([
        (
            "index.html",
            (
                "<html><script>boot()</script><body>app shell</body></html>",
                "text/html",
            ),
        ),
        ("_app/app.js", ("export {}", "text/javascript")),
    ]))));
    config
}

async fn get(api: &Api, path: &str, extra: &str) -> Reply {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\n{extra}Connection: close\r\n\r\n",
        api.host()
    );
    raw(api.addr, request.as_bytes()).await
}

#[tokio::test]
async fn the_app_loads_without_a_token_and_with_a_strict_policy() {
    let api = start_with(config()).await;
    for path in ["/", "/inbox", "/workspaces/some-id", "/_app/app.js"] {
        let reply = get(&api, path, "").await;
        assert_eq!(reply.status, 200, "{path}: {reply:?}");
        let csp = reply.header("content-security-policy").unwrap();
        assert!(csp.contains("default-src 'self'"), "{csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
        assert_eq!(reply.header("x-frame-options"), Some("DENY"));
        assert!(!reply.body.contains(&api.token));
    }
    let shell = get(&api, "/inbox", "").await;
    assert!(shell.body.contains("app shell"), "{shell:?}");
    assert!(
        shell
            .header("content-security-policy")
            .unwrap()
            .contains("'sha256-"),
        "the inline bootstrap script is allowed by hash"
    );
    let missing = get(&api, "/missing.png", "").await;
    assert_eq!(missing.status, 404);
}

#[tokio::test]
async fn the_api_and_every_write_still_need_the_token() {
    let api = start_with(config()).await;
    for path in [
        "/api/health",
        "/api/inbox",
        "/api/openapi.json",
        "/api/nothing",
    ] {
        let reply = get(&api, path, "").await;
        assert_eq!(reply.status, 401, "{path}: {reply:?}");
        assert_eq!(reply.error(), "unauthorized");
    }
    let post = format!(
        "POST /inbox HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        api.host()
    );
    assert_eq!(raw(api.addr, post.as_bytes()).await.status, 401);
    let with_token = api.get("/api/health").await;
    assert_eq!(with_token.status, 200);
}

#[tokio::test]
async fn the_app_keeps_the_host_and_origin_checks() {
    let api = start_with(config()).await;
    let rebinding = "GET / HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n";
    assert_eq!(raw(api.addr, rebinding.as_bytes()).await.status, 421);
    let foreign = get(&api, "/", "Origin: http://evil.example\r\n").await;
    assert_eq!(foreign.status, 403);
    let own = get(&api, "/", &format!("Origin: http://{}\r\n", api.host())).await;
    assert_eq!(own.status, 200);
}

#[tokio::test]
async fn without_an_app_everything_needs_the_token() {
    let mut off = ApiConfig::default();
    off.ui = None;
    let api = start_with(off).await;
    let reply = get(&api, "/", "").await;
    assert_eq!(reply.status, 401, "{reply:?}");
}

fn config_with_version(version_json: &'static str) -> ApiConfig {
    let mut config = ApiConfig::default();
    config.ui = Some(Arc::new(Files(HashMap::from([
        ("index.html", ("<html></html>", "text/html")),
        ("_app/version.json", (version_json, "application/json")),
    ]))));
    config
}

#[tokio::test]
async fn health_names_the_build_of_the_app_it_serves() {
    let api = start_with(config_with_version(r#"{"version":"1731456000000"}"#)).await;
    let health = api.get("/api/health").await.json();
    assert_eq!(health["ui_build_id"], "1731456000000");
    api.running.shutdown().await;
}

#[tokio::test]
async fn health_names_no_build_when_the_app_has_no_readable_version() {
    for version_json in ["not json", r#"{"version":7}"#, "{}"] {
        let api = start_with(config_with_version(version_json)).await;
        let health = api.get("/api/health").await.json();
        assert_eq!(
            health["ui_build_id"],
            serde_json::Value::Null,
            "{version_json}"
        );
        api.running.shutdown().await;
    }
    let api = start_with(config()).await;
    assert_eq!(
        api.get("/api/health").await.json()["ui_build_id"],
        serde_json::Value::Null,
        "an app build without a version file"
    );
    api.running.shutdown().await;
}
