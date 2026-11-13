// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;

use super::*;

#[derive(Debug)]
struct Fake(HashMap<&'static str, (&'static str, &'static str)>);

impl UiAssets for Fake {
    fn get(&self, path: &str) -> Option<UiFile> {
        self.0
            .get(path)
            .map(|(body, kind)| UiFile::new(body.as_bytes(), *kind))
    }
}

fn service() -> UiService {
    UiService::new(Arc::new(Fake(HashMap::from([
        (
            "index.html",
            (
                "<html><script src=\"/a.js\"></script><script>let a = 1;</script></html>",
                "text/html",
            ),
        ),
        ("_app/immutable/x.js", ("1", "text/javascript")),
        ("robots.txt", ("none", "text/plain")),
    ]))))
}

fn header<'a>(r: &'a Response, name: &str) -> &'a str {
    r.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

#[test]
fn hashes_only_inline_scripts() {
    let hashes = inline_script_hashes(
        "<script src=\"a.js\"></script><script>{ x }</script><script type=\"module\">y</script>",
    );
    assert_eq!(hashes.len(), 2);
    // sha256("{ x }"), base64.
    assert_eq!(
        hashes[0],
        STANDARD.encode(Sha256::digest(b"{ x }")),
        "{hashes:?}"
    );
    assert_eq!(inline_script_hashes("<script>unterminated").len(), 0);
    assert_eq!(inline_script_hashes("<script").len(), 0);
    assert_eq!(inline_script_hashes("no scripts").len(), 0);
}

#[test]
fn files_get_types_caching_and_security_headers() {
    let ui = service();
    let r = ui.respond(&Method::GET, "/robots.txt");
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "content-type"), "text/plain");
    assert_eq!(header(&r, "cache-control"), "no-cache");
    assert_eq!(header(&r, "x-frame-options"), "DENY");
    assert_eq!(header(&r, "x-content-type-options"), "nosniff");
    assert_eq!(header(&r, "referrer-policy"), "no-referrer");
    let csp = header(&r, "content-security-policy");
    assert!(csp.contains("default-src 'self'"), "{csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(csp.contains("script-src 'self' 'sha256-"), "{csp}");
    assert!(csp.contains("style-src-attr 'unsafe-inline'"), "{csp}");
    assert!(
        csp.contains("style-src-elem 'self' 'unsafe-inline'"),
        "{csp}"
    );
    assert!(!csp.contains("style-src 'unsafe-inline'"), "{csp}");
    assert!(!csp.contains("unsafe-eval"), "{csp}");
    let r = ui.respond(&Method::GET, "/_app/immutable/x.js");
    assert_eq!(
        header(&r, "cache-control"),
        "public, max-age=31536000, immutable"
    );
}

#[test]
fn routes_without_a_file_get_the_app_shell() {
    let ui = service();
    for path in ["/", "/inbox", "/workspaces/a/b"] {
        let r = ui.respond(&Method::GET, path);
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert_eq!(header(&r, "content-type"), "text/html", "{path}");
    }
    let r = ui.respond(&Method::GET, "/missing.js");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(header(&r, "content-type"), "application/json");
    assert!(r.headers().contains_key("content-security-policy"));
}

#[tokio::test]
async fn head_has_no_body_and_other_methods_are_not_routes() {
    let ui = service();
    let r = ui.respond(&Method::HEAD, "/robots.txt");
    assert_eq!(r.status(), StatusCode::OK);
    let body = axum::body::to_bytes(r.into_body(), 1024).await.unwrap();
    assert!(body.is_empty());
    let r = ui.respond(&Method::POST, "/inbox");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_build_without_index_answers_404_not_a_panic() {
    let ui = UiService::new(Arc::new(Fake(HashMap::new())));
    let r = ui.respond(&Method::GET, "/");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    let r = ui.respond(&Method::GET, "/inbox");
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
}

#[cfg(feature = "embedded-ui")]
#[test]
fn the_embedded_build_serves_its_index_when_it_was_built() {
    let ui = EmbeddedUi;
    assert!(ui.get("no/such/file.txt").is_none());
    if let Some(index) = ui.get("index.html") {
        assert!(index.content_type.starts_with("text/html"));
        let service = UiService::new(Arc::new(ui));
        assert!(service.csp.to_str().unwrap().contains("'sha256-"));
    }
}
