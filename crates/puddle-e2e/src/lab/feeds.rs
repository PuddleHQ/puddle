// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fake private package feeds for [`WebHost`](super::WebHost): one feed each for npm, `NuGet`, pip
//! and Maven that answer `401` to a request without the one token it knows and serve a tiny
//! package to a request that carries it, so a test can have the real tools install from a feed
//! that needs a credential. The token is accepted as a bearer token (npm) or as the password of
//! basic authentication (the others). Test code only.

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flate2::Compression;
use flate2::write::GzEncoder;

use super::pip_index::{wheel, wheel_name, zip_stored};
use super::{Responder, Seen, WebAnswer};

/// The package every feed serves.
pub const NPM_PACKAGE: &str = "probe-private";
/// The `NuGet` package id every feed serves.
pub const NUGET_PACKAGE: &str = "Probe.Private";
/// The Python project the pip feed serves.
pub const PIP_PROJECT: &str = "probe-private";
/// The Maven coordinates the Maven feed serves, `group:artifact:version`.
pub const MAVEN_COORDINATES: &str = "com.example:probe-private:1.0";

/// What a request carried as a credential: a bearer token, or the password of basic
/// authentication.
fn presented(seen: &Seen) -> Option<String> {
    let value = seen.headers_named("authorization").into_iter().next()?;
    if let Some(token) = value.strip_prefix("Bearer ") {
        return Some(token.to_owned());
    }
    let decoded = STANDARD.decode(value.strip_prefix("Basic ")?).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    text.split_once(':')
        .map(|(_, password)| password.to_owned())
}

fn answer(content_type: &'static str, body: Vec<u8>) -> WebAnswer {
    WebAnswer {
        status: 200,
        content_type,
        body,
    }
}

fn status(status: u16) -> WebAnswer {
    WebAnswer {
        status,
        content_type: "text/plain",
        body: format!("{status}\n").into_bytes(),
    }
}

/// A ustar archive of `files`, gzip-compressed (an npm tarball).
fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    for (name, data) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        archive.append_data(&mut header, name, *data).unwrap();
    }
    let raw = archive.into_inner().unwrap();
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    gz.write_all(&raw).unwrap();
    gz.finish().unwrap()
}

fn npm_answer(host: &str, path: &str) -> WebAnswer {
    let tarball = format!("/{NPM_PACKAGE}/-/{NPM_PACKAGE}-1.0.0.tgz");
    if path == format!("/{NPM_PACKAGE}") {
        let doc = serde_json::json!({
            "name": NPM_PACKAGE,
            "dist-tags": {"latest": "1.0.0"},
            "versions": {"1.0.0": {
                "name": NPM_PACKAGE,
                "version": "1.0.0",
                "dist": {"tarball": format!("https://{host}{tarball}")},
            }},
        });
        answer("application/json", doc.to_string().into_bytes())
    } else if path == tarball {
        let manifest =
            format!("{{\"name\":\"{NPM_PACKAGE}\",\"version\":\"1.0.0\",\"main\":\"index.js\"}}");
        answer(
            "application/octet-stream",
            tgz(&[
                ("package/package.json", manifest.as_bytes()),
                (
                    "package/index.js",
                    b"module.exports = 'from the private feed';\n",
                ),
            ]),
        )
    } else {
        status(404)
    }
}

fn nuget_answer(host: &str, path: &str) -> WebAnswer {
    let id = NUGET_PACKAGE.to_lowercase();
    let flat = format!("/v3/flat/{id}");
    if path == "/v3/index.json" {
        let doc = serde_json::json!({
            "version": "3.0.0",
            "resources": [{
                "@id": format!("https://{host}/v3/flat/"),
                "@type": "PackageBaseAddress/3.0.0",
            }],
        });
        answer("application/json", doc.to_string().into_bytes())
    } else if path == format!("{flat}/index.json") {
        answer("application/json", br#"{"versions":["1.0.0"]}"#.to_vec())
    } else if path == format!("{flat}/1.0.0/{id}.1.0.0.nupkg") {
        let nuspec = format!(
            "<?xml version=\"1.0\"?><package xmlns=\"http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd\">\
             <metadata><id>{NUGET_PACKAGE}</id><version>1.0.0</version><authors>lab</authors>\
             <description>a package from the private feed</description></metadata></package>"
        );
        answer(
            "application/octet-stream",
            zip_stored(&[
                (format!("{NUGET_PACKAGE}.nuspec"), nuspec.into_bytes()),
                ("lib/netstandard2.0/_._".to_owned(), Vec::new()),
            ]),
        )
    } else {
        status(404)
    }
}

fn pip_answer(path: &str) -> WebAnswer {
    let name = wheel_name(PIP_PROJECT);
    if path == format!("/simple/{PIP_PROJECT}/") {
        answer(
            "text/html",
            format!(
                "<!DOCTYPE html><html><body><a href=\"/files/{name}\">{name}</a></body></html>\n"
            )
            .into_bytes(),
        )
    } else if path == format!("/files/{name}") {
        answer("application/octet-stream", wheel(PIP_PROJECT))
    } else {
        status(404)
    }
}

fn maven_answer(path: &str) -> WebAnswer {
    let base = "/m2/com/example/probe-private/1.0/probe-private-1.0";
    if path == format!("{base}.pom") {
        answer(
            "application/xml",
            b"<project xmlns=\"http://maven.apache.org/POM/4.0.0\"><modelVersion>4.0.0</modelVersion>\
              <groupId>com.example</groupId><artifactId>probe-private</artifactId><version>1.0</version></project>"
                .to_vec(),
        )
    } else if path == format!("{base}.jar") {
        answer(
            "application/java-archive",
            zip_stored(&[(
                "META-INF/MANIFEST.MF".to_owned(),
                b"Manifest-Version: 1.0\n".to_vec(),
            )]),
        )
    } else {
        status(404)
    }
}

/// The answers of four private feeds on their own hosts: npm on `npm`, `NuGet` on `nuget`, pip on
/// `pip` and Maven on `maven`. A request to one of them without `token` gets `401` (with a basic
/// challenge); with it, the one package of the feed (`NPM_PACKAGE`, `NUGET_PACKAGE`,
/// `PIP_PROJECT`, `MAVEN_COORDINATES`) or `404`. A request to any other host is left alone.
#[must_use]
pub fn private_feeds(
    npm: &'static str,
    nuget: &'static str,
    pip: &'static str,
    maven: &'static str,
    token: &'static str,
) -> Responder {
    Arc::new(move |seen| {
        let host = seen
            .headers_named("host")
            .first()
            .map(|h| h.split(':').next().unwrap_or_default().to_owned())?;
        if ![npm, nuget, pip, maven].contains(&host.as_str()) {
            return None;
        }
        if presented(seen).as_deref() != Some(token) {
            return Some(status(401));
        }
        let path = seen.target.split('?').next().unwrap_or_default();
        Some(match host.as_str() {
            h if h == npm => npm_answer(h, path),
            h if h == nuget => nuget_answer(h, path),
            h if h == pip => pip_answer(path),
            _ => maven_answer(path),
        })
    })
}

#[cfg(test)]
mod tests;
