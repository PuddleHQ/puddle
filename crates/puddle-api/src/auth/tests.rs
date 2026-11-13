// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::http::HeaderValue;

use super::*;

const PORT: u16 = 4321;

fn guard() -> (Guard, ApiToken) {
    let token = ApiToken::generate().unwrap();
    (
        Guard::new(token.clone(), PORT, &["tauri://localhost".to_owned()]),
        token,
    )
}

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut h = HeaderMap::new();
    for (k, v) in pairs {
        h.append(
            header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
            HeaderValue::from_str(v).unwrap(),
        );
    }
    h
}

fn check(pairs: &[(&str, &str)]) -> Result<(), Refusal> {
    let (g, _) = guard();
    g.check(&Uri::from_static("/api/health"), &headers(pairs))
}

#[test]
fn host_must_be_our_loopback_address_and_port() {
    let (g, token) = guard();
    let auth = format!("Bearer {}", token.expose());
    let uri = Uri::from_static("/api/health");
    for ok in ["127.0.0.1:4321", "localhost:4321", "LOCALHOST:4321"] {
        assert_eq!(
            g.check(&uri, &headers(&[("host", ok), ("authorization", &auth)])),
            Ok(()),
            "{ok}"
        );
    }
    for bad in [
        "127.0.0.1",
        "localhost",
        "127.0.0.1:4322",
        "[::1]:4321",
        "evil.example:4321",
        "127.0.0.1.nip.io:4321",
        "",
    ] {
        assert_eq!(
            g.check(&uri, &headers(&[("host", bad), ("authorization", &auth)])),
            Err(Refusal::Host),
            "{bad}"
        );
    }
    assert_eq!(
        g.check(&uri, &headers(&[("authorization", &auth)])),
        Err(Refusal::Host)
    );
    assert_eq!(
        check(&[("host", "127.0.0.1:4321"), ("host", "127.0.0.1:4321")]),
        Err(Refusal::Host)
    );
    let non_utf8 = {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_bytes(b"\xff:4321").unwrap());
        h
    };
    assert_eq!(g.check(&uri, &non_utf8), Err(Refusal::Host));
}

#[test]
fn an_absolute_target_must_name_us_too() {
    let (g, token) = guard();
    let auth = format!("Bearer {}", token.expose());
    let h = headers(&[("host", "127.0.0.1:4321"), ("authorization", &auth)]);
    let evil: Uri = "http://evil.example/api/health".parse().unwrap();
    assert_eq!(g.check(&evil, &h), Err(Refusal::Host));
    let own: Uri = "http://127.0.0.1:4321/api/health".parse().unwrap();
    assert_eq!(g.check(&own, &h), Ok(()));
}

#[test]
fn origin_when_present_must_be_ours_or_configured() {
    let (g, token) = guard();
    let auth = format!("Bearer {}", token.expose());
    let uri = Uri::from_static("/api/health");
    let with = |origin: &str| {
        g.check(
            &uri,
            &headers(&[
                ("host", "127.0.0.1:4321"),
                ("origin", origin),
                ("authorization", &auth),
            ]),
        )
    };
    for ok in [
        "http://127.0.0.1:4321",
        "http://localhost:4321",
        "tauri://localhost",
    ] {
        assert_eq!(with(ok), Ok(()), "{ok}");
    }
    for bad in [
        "null",
        "http://evil.example",
        "http://127.0.0.1:4322",
        "https://127.0.0.1:4321",
        "http://127.0.0.1:4321/",
        "http://localhost",
    ] {
        assert_eq!(with(bad), Err(Refusal::Origin), "{bad}");
    }
    assert_eq!(
        g.check(
            &uri,
            &headers(&[
                ("host", "127.0.0.1:4321"),
                ("origin", "http://127.0.0.1:4321"),
                ("origin", "http://127.0.0.1:4321"),
                ("authorization", &auth),
            ])
        ),
        Err(Refusal::Origin)
    );
}

#[test]
fn token_must_be_a_single_exact_bearer_header() {
    let (g, token) = guard();
    let uri = Uri::from_static("/api/health");
    let with = |values: &[&str]| {
        let mut pairs = vec![("host", "127.0.0.1:4321")];
        pairs.extend(values.iter().map(|v| ("authorization", *v)));
        g.check(&uri, &headers(&pairs))
    };
    let t = token.expose();
    assert_eq!(with(&[&format!("Bearer {t}")]), Ok(()));
    assert_eq!(with(&[&format!("bearer {t}")]), Ok(()));
    for bad in [
        String::new(),
        "Bearer".to_owned(),
        "Bearer ".to_owned(),
        format!("Basic {t}"),
        format!("Bearer  {t}"),
        format!("Bearer {t} "),
        format!("Bearer {}", &t[..63]),
        format!("Bearer {}", "0".repeat(64)),
        t.to_owned(),
    ] {
        assert_eq!(with(&[&bad]), Err(Refusal::Token), "{bad:?}");
    }
    assert_eq!(with(&[]), Err(Refusal::Token));
    let good = format!("Bearer {t}");
    assert_eq!(with(&[&good, &good]), Err(Refusal::Token));
}

#[test]
fn host_is_checked_before_origin_before_token() {
    assert_eq!(
        check(&[("host", "evil:1"), ("origin", "http://evil")]),
        Err(Refusal::Host)
    );
    assert_eq!(
        check(&[("host", "127.0.0.1:4321"), ("origin", "http://evil")]),
        Err(Refusal::Origin)
    );
    assert_eq!(check(&[("host", "127.0.0.1:4321")]), Err(Refusal::Token));
}

#[test]
fn refusals_map_to_statuses_and_codes() {
    for (refusal, status, code) in [
        (
            Refusal::Host,
            StatusCode::MISDIRECTED_REQUEST,
            ErrorCode::MisdirectedHost,
        ),
        (
            Refusal::Origin,
            StatusCode::FORBIDDEN,
            ErrorCode::ForbiddenOrigin,
        ),
        (
            Refusal::Token,
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
        ),
    ] {
        let err = ApiError::from(refusal);
        assert_eq!(err.status(), status);
        assert_eq!(err.code(), code);
    }
}

fn remote_guard(listener: Listener) -> (Guard, ApiToken) {
    let remote = RemoteServing::new(
        vec![crate::remote::PublicHost::parse("puddle.example.org").unwrap()],
        None,
    )
    .unwrap();
    let (guard, token) = guard();
    (guard.remote(&remote, listener), token)
}

#[test]
fn remote_serving_off_knows_no_public_name() {
    let (g, token) = guard();
    let auth = format!("Bearer {}", token.expose());
    let uri = Uri::from_static("/api/health");
    let h = headers(&[("host", "puddle.example.org"), ("authorization", &auth)]);
    assert_eq!(g.check(&uri, &h), Err(Refusal::Host));
    let fetch = headers(&[
        ("host", "127.0.0.1:4321"),
        ("origin", "tauri://localhost"),
        ("sec-fetch-site", "cross-site"),
        ("authorization", &auth),
    ]);
    assert_eq!(
        g.check(&uri, &fetch),
        Ok(()),
        "fetch metadata is not read when remote is off"
    );
}

#[test]
fn a_public_host_never_takes_the_install_token_and_a_loopback_host_still_does() {
    let (g, token) = remote_guard(Listener::Loopback);
    let auth = format!("Bearer {}", token.expose());
    let uri = Uri::from_static("/api/health");
    let public = |extra: &[(&str, &str)]| {
        let mut pairs = vec![("host", "puddle.example.org")];
        pairs.extend_from_slice(extra);
        g.check(&uri, &headers(&pairs))
    };
    assert_eq!(
        public(&[("authorization", &auth)]),
        Err(Refusal::CredentialHost)
    );
    assert_eq!(
        public(&[("authorization", "Bearer nope")]),
        Err(Refusal::Token)
    );
    assert_eq!(public(&[]), Err(Refusal::Token));
    let own = headers(&[("host", "127.0.0.1:4321"), ("authorization", &auth)]);
    assert_eq!(g.check(&uri, &own), Ok(()));
}

#[test]
fn the_network_listener_has_no_loopback_names() {
    let (g, token) = remote_guard(Listener::Network);
    let auth = format!("Bearer {}", token.expose());
    let uri = Uri::from_static("/api/health");
    for host in ["127.0.0.1:4321", "localhost:4321", "evil.example"] {
        let h = headers(&[("host", host), ("authorization", &auth)]);
        assert_eq!(g.check(&uri, &h), Err(Refusal::Host), "{host}");
    }
    let h = headers(&[("host", "puddle.example.org"), ("authorization", &auth)]);
    assert_eq!(g.check(&uri, &h), Err(Refusal::CredentialHost));
    // An absolute-form target must be of the same kind as the Host.
    let loopback: Uri = "http://127.0.0.1:4321/api/health".parse().unwrap();
    assert_eq!(g.check(&loopback, &h), Err(Refusal::Host));
    let (lg, ltoken) = remote_guard(Listener::Loopback);
    let lauth = format!("Bearer {}", ltoken.expose());
    let lh = headers(&[("host", "puddle.example.org"), ("authorization", &lauth)]);
    assert_eq!(lg.check(&loopback, &lh), Err(Refusal::Host));
    let public: Uri = "https://puddle.example.org/api/health".parse().unwrap();
    assert_eq!(lg.check(&public, &lh), Err(Refusal::CredentialHost));
}

#[test]
fn behind_a_public_name_an_origin_is_the_public_one_and_fetch_metadata_says_same_origin() {
    let (g, _) = remote_guard(Listener::Loopback);
    let uri = Uri::from_static("/");
    let with = |origin: &str, site: Option<&str>| {
        let mut pairs = vec![("host", "puddle.example.org"), ("origin", origin)];
        pairs.extend(site.map(|s| ("sec-fetch-site", s)));
        g.check_origin(
            &headers(&pairs),
            g.check_host(&uri, &headers(&pairs)).unwrap(),
        )
    };
    assert_eq!(with("https://puddle.example.org", None), Ok(()));
    assert_eq!(
        with("https://puddle.example.org", Some("same-origin")),
        Ok(())
    );
    assert_eq!(
        with("https://puddle.example.org", Some("Same-Origin")),
        Ok(())
    );
    for site in ["cross-site", "same-site", "none", ""] {
        assert_eq!(
            with("https://puddle.example.org", Some(site)),
            Err(Refusal::FetchSite),
            "{site:?}"
        );
    }
    for origin in [
        "https://evil.example",
        "http://puddle.example.org",
        "tauri://localhost",
        "http://127.0.0.1:4321",
    ] {
        assert_eq!(
            with(origin, Some("same-origin")),
            Err(Refusal::Origin),
            "{origin}"
        );
    }
}

#[test]
fn the_new_refusals_have_their_own_log_reason_and_answer() {
    assert_eq!(Refusal::FetchSite.reason(), "fetch-site");
    assert_eq!(Refusal::CredentialHost.reason(), "credential-host");
    let error = ApiError::from(Refusal::FetchSite);
    assert_eq!(error.status(), StatusCode::FORBIDDEN);
    let error = ApiError::from(Refusal::CredentialHost);
    assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
}
