// SPDX-License-Identifier: GPL-3.0-or-later
//! Serving the single-page app from the API's own origin. The page and the API
//! share one origin, so there is no CORS (the API's `Origin` check stays intact), SSE works, and the shell and a
//! browser tab run identical code.
//!
//! Static files need no token (the page has to load before it can present one) but keep the
//! `Host` and `Origin` checks. They are served with a strict `Content-Security-Policy` and no
//! framing. The inline scripts in the build (`SvelteKit`'s bootstrap, which changes with every
//! build, and the density hint from `app.html`) are allowed by hashes computed from `index.html`
//! when the server starts.

use std::borrow::Cow;
use std::fmt::Debug;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

use crate::error::ApiError;

/// One file of the app.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UiFile {
    /// The bytes.
    pub body: Cow<'static, [u8]>,
    /// The `Content-Type`.
    pub content_type: String,
}

impl UiFile {
    /// A file of `body` served as `content_type`.
    #[must_use]
    pub fn new(body: impl Into<Cow<'static, [u8]>>, content_type: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            content_type: content_type.into(),
        }
    }
}

/// Where the app's files come from: the embedded build, or a fake in tests.
pub trait UiAssets: Debug + Send + Sync {
    /// The file at `path` (relative, no leading slash, e.g. `_app/version.json`), if any.
    fn get(&self, path: &str) -> Option<UiFile>;
}

/// The build embedded in the binary (feature `embedded-ui`): `ui/build` as it was at compile time.
#[cfg(feature = "embedded-ui")]
#[derive(Debug, Clone, Copy, Default)]
pub struct EmbeddedUi;

#[cfg(feature = "embedded-ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "../../ui/build/"]
#[allow_missing = true]
struct Build;

#[cfg(feature = "embedded-ui")]
impl UiAssets for EmbeddedUi {
    fn get(&self, path: &str) -> Option<UiFile> {
        let file = <Build as rust_embed::Embed>::get(path)?;
        Some(UiFile::new(file.data, file.metadata.mimetype().to_owned()))
    }
}

/// The identity of the app build `assets` holds: the `version` in `_app/version.json`, which the
/// build writes and the app itself carries (`$app/environment`'s `version`). `None` when the
/// assets have no such file or it is not the shape the build writes.
pub(crate) fn build_id(assets: &dyn UiAssets) -> Option<String> {
    let file = assets.get("_app/version.json")?;
    let json: serde_json::Value = serde_json::from_slice(&file.body).ok()?;
    json.get("version")?.as_str().map(str::to_owned)
}

/// The app's files plus the headers they are served with.
#[derive(Clone)]
pub(crate) struct UiService {
    assets: Arc<dyn UiAssets>,
    csp: HeaderValue,
    /// Whether to send `Cross-Origin-Opener-Policy: same-origin` (remote serving).
    opener_policy: bool,
}

const INDEX: &str = "index.html";

impl UiService {
    pub(crate) fn new(assets: Arc<dyn UiAssets>) -> Self {
        let scripts = assets
            .get(INDEX)
            .map(|index| inline_script_hashes(&String::from_utf8_lossy(&index.body)))
            .unwrap_or_default();
        let csp = content_security_policy(&scripts);
        Self {
            assets,
            csp,
            opener_policy: false,
        }
    }

    /// Adds `Cross-Origin-Opener-Policy: same-origin` to every file, so a page a guest opened
    /// with a link to the UI cannot navigate the UI's tab away. Sent only with remote serving,
    /// where a browser other than the desktop app's loads the UI.
    pub(crate) fn with_opener_policy(mut self, on: bool) -> Self {
        self.opener_policy = on;
        self
    }

    /// Answers a request that is not for `/api`.
    pub(crate) fn respond(&self, method: &Method, path: &str) -> Response {
        if method != Method::GET && method != Method::HEAD {
            return ApiError::not_found("no such route").into_response();
        }
        let relative = path.trim_start_matches('/');
        let file = if relative.is_empty() {
            self.assets.get(INDEX)
        } else {
            self.assets.get(relative).or_else(|| {
                // A path with an extension is a missing file; any other path is a client-side
                // route, answered by the app shell.
                let last = relative.rsplit('/').next().unwrap_or_default();
                (!last.contains('.'))
                    .then(|| self.assets.get(INDEX))
                    .flatten()
            })
        };
        let Some(file) = file else {
            return self.with_security_headers(
                ApiError::not_found("no such file (is the UI built?)").into_response(),
            );
        };
        let immutable = relative.starts_with("_app/immutable/");
        let mut response = Response::new(if method == Method::HEAD {
            Body::empty()
        } else {
            Body::from(file.body.into_owned())
        });
        let headers = response.headers_mut();
        if let Ok(value) = HeaderValue::from_str(&file.content_type) {
            headers.insert(header::CONTENT_TYPE, value);
        }
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            }),
        );
        *response.status_mut() = StatusCode::OK;
        self.with_security_headers(response)
    }

    fn with_security_headers(&self, mut response: Response) -> Response {
        const NOSNIFF: HeaderName = header::X_CONTENT_TYPE_OPTIONS;
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_SECURITY_POLICY, self.csp.clone());
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(NOSNIFF, HeaderValue::from_static("nosniff"));
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        if self.opener_policy {
            headers.insert(
                HeaderName::from_static("cross-origin-opener-policy"),
                HeaderValue::from_static("same-origin"),
            );
        }
        response
    }
}

/// `default-src 'self'` with the page's own inline scripts allowed by hash, and no framing.
/// `style-src-attr 'unsafe-inline'` is for `SvelteKit`'s route announcer, which hides itself with
/// a `style` attribute (blocked, it would show its text on screen); an inline style can't run
/// script. `style-src-elem 'self' 'unsafe-inline'` is for the terminal emulator, which writes its
/// theme and cell sizes into `<style>` elements it creates (no nonce option; blocked, the screen
/// draws with the wrong colours and sizes). Style text can't run script, and `default-src 'self'`
/// still closes every way for a stylesheet to fetch from elsewhere; `style-src` stays `'self'`.
fn content_security_policy(script_hashes: &[String]) -> HeaderValue {
    let mut scripts = String::from("'self'");
    for hash in script_hashes {
        scripts.push_str(" 'sha256-");
        scripts.push_str(hash);
        scripts.push('\'');
    }
    let policy = format!(
        "default-src 'self'; script-src {scripts}; style-src-attr 'unsafe-inline'; \
         style-src-elem 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    HeaderValue::from_str(&policy)
        .unwrap_or_else(|_| HeaderValue::from_static("default-src 'self'"))
}

/// Base64 SHA-256 of every inline `<script>` body in `html` (the form a CSP hash source takes).
fn inline_script_hashes(html: &str) -> Vec<String> {
    let mut hashes = Vec::new();
    let mut rest = html;
    while let Some((_, after)) = rest.split_once("<script") {
        let Some((attributes, body)) = after.split_once('>') else {
            break;
        };
        let Some((script, next)) = body.split_once("</script>") else {
            break;
        };
        if !attributes.contains("src=") {
            hashes.push(STANDARD.encode(Sha256::digest(script.as_bytes())));
        }
        rest = next;
    }
    hashes
}

#[cfg(test)]
mod tests;
