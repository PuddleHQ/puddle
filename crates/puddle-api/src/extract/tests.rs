// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_buffer_keeps_every_byte_while_it_grows_past_its_first_size() {
    let mut buf = Zeroizing::new(Vec::with_capacity(4));
    let mut want = Vec::new();
    for chunk in [&b"ab"[..], b"cdef", b"", b"ghijklmnopqrstu", b"v"] {
        push(&mut buf, chunk);
        want.extend_from_slice(chunk);
        assert_eq!(*buf, want);
    }
    assert!(buf.capacity() >= want.len());
}

#[test]
fn only_json_content_types_are_taken() {
    let with = |value: &str| {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, value.parse().unwrap());
        is_json(&headers)
    };
    assert!(!is_json(&HeaderMap::new()));
    assert!(with("application/json"));
    assert!(with("Application/JSON; charset=utf-8"));
    assert!(with("application/vnd.api+json"));
    assert!(!with("text/json"));
    assert!(!with("application/x-www-form-urlencoded"));
}

#[tokio::test]
async fn a_body_that_breaks_off_is_a_bad_request_and_one_over_the_limit_is_too_large() {
    let broken = axum::body::Body::from_stream(futures_util::stream::iter([
        Ok(axum::body::Bytes::from_static(b"{\"a\"")),
        Err(std::io::Error::other("connection reset")),
    ]));
    let err = collect(broken, None).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);

    let big = axum::body::Body::new(http_body_util::Limited::new(
        axum::body::Body::from(vec![b'x'; 100]),
        10,
    ));
    let err = collect(big, Some(100)).await.unwrap_err();
    assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let fine = axum::body::Body::from("{\"a\":1}");
    assert_eq!(&**collect(fine, None).await.unwrap(), b"{\"a\":1}");
}

/// Every `.rs` file under `dir`, test files left out, as (path, text).
fn sources(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                let text = std::fs::read_to_string(&path).unwrap();
                out.push((path.display().to_string(), text));
            }
        }
    }
    out
}

/// The wire types that hold a [`SecretText`](crate::wire::SecretText), directly or through
/// another such type: a top-level `struct` or `enum` whose body (up to its closing `}` in the
/// first column) names one.
fn types_holding_a_secret(files: &[(String, String)]) -> std::collections::BTreeSet<String> {
    let mut bodies = Vec::new();
    for (_, text) in files {
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            let Some(rest) = [
                "pub struct ",
                "pub enum ",
                "pub(crate) struct ",
                "pub(crate) enum ",
            ]
            .iter()
            .find_map(|start| line.strip_prefix(start)) else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let mut body = line.to_owned();
            if !line.ends_with(';') {
                for inner in lines.by_ref() {
                    body.push('\n');
                    body.push_str(inner);
                    if inner == "}" {
                        break;
                    }
                }
            }
            bodies.push((name, body));
        }
    }
    let names = |body: &str, name: &str| {
        body.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == name)
    };
    let mut secret: std::collections::BTreeSet<String> = ["SecretText".to_owned()].into();
    loop {
        let before = secret.len();
        for (name, body) in &bodies {
            if secret.iter().any(|s| s != name && names(body, s)) {
                secret.insert(name.clone());
            }
        }
        if secret.len() == before {
            return secret;
        }
    }
}

/// Every body a handler takes, as (file, extractor, type): a parameter written
/// `pattern: path::Extractor<Type>` whose extractor's name ends in `Json`.
fn json_bodies(files: &[(String, String)]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (file, text) in files {
        for (at, _) in text.match_indices("): ") {
            let rest = &text[at + 3..];
            let path: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            let extractor = path.rsplit("::").next().unwrap_or_default();
            let Some(inner) = rest[path.len()..].strip_prefix('<') else {
                continue;
            };
            if !extractor.ends_with("Json") {
                continue;
            }
            let ty: String = inner
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            out.push((file.clone(), extractor.to_owned(), ty));
        }
    }
    out
}

/// A request body that holds a secret is read with [`SecretJson`], which wipes its buffer, never
/// with [`Json`], which leaves the secret in a `Bytes` copy and a `Value` tree. A source check:
/// it sees the handlers under `routes/` and the types under `wire/`, and a body read some other
/// way (by hand, or with an extractor whose name does not end in `Json`) is not seen.
#[test]
fn every_request_body_that_holds_a_secret_is_read_with_secret_json() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut wire = sources(&src.join("wire"));
    wire.extend(
        sources(&src)
            .into_iter()
            .filter(|(f, _)| f.ends_with("wire.rs")),
    );
    let secret = types_holding_a_secret(&wire);
    for known in [
        "StoreTokenRequest",
        "EnvSetRequest",
        "ChangePasswordRequest",
        "ProxySignInRequest",
    ] {
        assert!(secret.contains(known), "{known} not found in {secret:?}");
    }
    let bodies = json_bodies(&sources(&src.join("routes")));
    let secret_reads = bodies.iter().filter(|(_, e, _)| e == "SecretJson").count();
    let plain_reads = bodies.iter().filter(|(_, e, _)| e != "SecretJson").count();
    assert!(
        secret_reads >= 7 && plain_reads >= 40,
        "the scan finds the handlers: {bodies:?}"
    );
    let wrong: Vec<_> = bodies
        .iter()
        .filter(|(_, extractor, ty)| extractor != "SecretJson" && secret.contains(ty))
        .collect();
    assert!(wrong.is_empty(), "read with plain Json: {wrong:?}");
}
