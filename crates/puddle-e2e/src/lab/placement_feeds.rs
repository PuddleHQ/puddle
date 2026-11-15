// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fake private feeds whose clients do not send the token in a plain header, so a test can ask,
//! per client, where the token travels and whether Puddle's swap reaches it. Each feed answers
//! `401` to a request without the one token it knows:
//!
//! - **path**: an npm registry that takes the token as the first path segment pair
//!   (`/t/<token>/<package>`), the way a feed with a token in its URL does;
//! - **query**: the wheel of the pip feed as a direct download that takes the token as the query
//!   parameter [`QUERY_PARAMETER`] (a signed or private release link);
//! - **cargo**: a sparse `cargo` registry that requires authentication; `cargo` sends the
//!   registry token as the whole `authorization` value;
//! - **go**: a Go module proxy that takes the token as the password of basic authentication (from
//!   `.netrc` or from the address's user information).
//!
//! The `401` carries a basic challenge, as in [`private_feeds`](super::private_feeds). Test code
//! only.

use std::fmt::Write as _;
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::feeds::{PIP_PROJECT, answer, npm_answer, pip_answer, presented, status, tgz};
use super::pip_index::{wheel_name, zip_stored};
use super::{Responder, Seen, WebAnswer};

/// The crate the cargo feed serves.
pub const CARGO_CRATE: &str = "probe-private";
/// The Go module the proxy serves.
pub const GO_MODULE: &str = "example.com/probe/private";
/// The query parameter the direct-download feed reads its token from.
pub const QUERY_PARAMETER: &str = "access_token";

/// The host names of the four feeds.
#[derive(Clone, Copy, Debug)]
pub struct PlacementHosts {
    /// The npm registry with the token in the path.
    pub path: &'static str,
    /// The direct download with the token in the query.
    pub query: &'static str,
    /// The sparse `cargo` registry.
    pub cargo: &'static str,
    /// The Go module proxy.
    pub go: &'static str,
}

/// The file name of the wheel the query feed serves.
#[must_use]
pub fn wheel_file() -> String {
    wheel_name(PIP_PROJECT)
}

fn crate_file() -> Vec<u8> {
    let manifest =
        format!("[package]\nname = \"{CARGO_CRATE}\"\nversion = \"1.0.0\"\nedition = \"2021\"\n");
    tgz(&[
        (
            &format!("{CARGO_CRATE}-1.0.0/Cargo.toml"),
            manifest.as_bytes(),
        ),
        (
            &format!("{CARGO_CRATE}-1.0.0/src/lib.rs"),
            b"pub fn from_the_private_feed() {}\n",
        ),
    ])
}

fn cargo_answer(host: &str, path: &str) -> WebAnswer {
    let index = format!("/pr/ob/{CARGO_CRATE}");
    let download = format!("/dl/{CARGO_CRATE}/1.0.0/download");
    if path == "/config.json" {
        let doc = serde_json::json!({
            "dl": format!("https://{host}/dl"),
            "api": format!("https://{host}"),
            "auth-required": true,
        });
        answer("application/json", doc.to_string().into_bytes())
    } else if path == index {
        let sum = Sha256::digest(crate_file())
            .iter()
            .fold(String::new(), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            });
        let line = serde_json::json!({
            "name": CARGO_CRATE,
            "vers": "1.0.0",
            "deps": [],
            "cksum": sum,
            "features": {},
            "yanked": false,
        });
        answer("text/plain", format!("{line}\n").into_bytes())
    } else if path == download {
        answer("application/octet-stream", crate_file())
    } else {
        status(404)
    }
}

fn go_answer(path: &str) -> WebAnswer {
    let base = format!("/{GO_MODULE}/@v");
    let zip_root = format!("{GO_MODULE}@v1.0.0");
    let module_file = format!("module {GO_MODULE}\n\ngo 1.18\n");
    if path == format!("{base}/list") {
        answer("text/plain", b"v1.0.0\n".to_vec())
    } else if path == format!("{base}/v1.0.0.info") || path == format!("/{GO_MODULE}/@latest") {
        answer(
            "application/json",
            br#"{"Version":"v1.0.0","Time":"2020-01-01T00:00:00Z"}"#.to_vec(),
        )
    } else if path == format!("{base}/v1.0.0.mod") {
        answer("text/plain", module_file.into_bytes())
    } else if path == format!("{base}/v1.0.0.zip") {
        answer(
            "application/zip",
            zip_stored(&[
                (format!("{zip_root}/go.mod"), module_file.into_bytes()),
                (
                    format!("{zip_root}/probe.go"),
                    b"package private\n\n// FromThePrivateFeed marks the module.\nconst FromThePrivateFeed = true\n"
                        .to_vec(),
                ),
            ]),
        )
    } else {
        status(404)
    }
}

/// The value of query parameter `name` in a request target (no decoding: the tokens of a test
/// are plain).
fn query_value<'a>(target: &'a str, name: &str) -> Option<&'a str> {
    let (_, query) = target.split_once('?')?;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
}

/// The answers of the four feeds of `hosts` for `token`. A request to one of them that lacks the
/// token where that feed reads it gets `401`, whatever else it carries (so a stand-in left in the
/// place is refused); with it, the one package of the feed or `404`. A request to any other host
/// is left alone.
#[must_use]
pub fn placement_feeds(hosts: PlacementHosts, token: &'static str) -> Responder {
    Arc::new(move |seen: &Seen| {
        let host = seen
            .headers_named("host")
            .first()
            .map(|h| h.split(':').next().unwrap_or_default().to_owned())?;
        let (path, _) = seen
            .target
            .split_once('?')
            .unwrap_or((seen.target.as_str(), ""));
        if host == hosts.path {
            let prefix = format!("/t/{token}");
            return Some(match path.strip_prefix(&prefix) {
                Some(rest) if rest.starts_with('/') => npm_answer(&host, &prefix, rest),
                _ => status(401),
            });
        }
        if host == hosts.query {
            if query_value(&seen.target, QUERY_PARAMETER) != Some(token) {
                return Some(status(401));
            }
            return Some(pip_answer(path));
        }
        if host == hosts.cargo || host == hosts.go {
            if presented(seen).as_deref() != Some(token) {
                return Some(status(401));
            }
            return Some(if host == hosts.cargo {
                cargo_answer(&host, path)
            } else {
                go_answer(path)
            });
        }
        None
    })
}

#[cfg(test)]
mod tests;
