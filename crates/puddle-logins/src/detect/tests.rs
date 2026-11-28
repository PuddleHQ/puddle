// SPDX-License-Identifier: AGPL-3.0-or-later
//! The corpus: values found in real answers and requests, split into tokens the detector must
//! find and non-tokens it must leave alone. Token-shaped samples are built at run time from a
//! prefix and a generated body so that no literal in the source is a credential.

use super::*;

/// `len` base62 characters from a seed: random-looking, deterministic, never a real credential.
fn body(seed: u64, len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut state = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let index = usize::try_from((state >> 33) % 62).unwrap();
            char::from(ALPHABET[index])
        })
        .collect()
}

fn jwt() -> String {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = engine.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = engine.encode(br#"{"sub":"1234","exp":1893456000}"#);
    format!("{header}.{payload}.{}", body(7, 43))
}

#[test]
fn a_documented_prefix_is_a_token_in_any_field() {
    let samples = [
        ("ghp_", 36),
        ("gho_", 36),
        ("github_pat_", 60),
        ("sk-ant-", 90),
        ("sk-proj-", 60),
        ("xoxb-", 40),
        ("glpat-", 20),
        ("npm_", 36),
        ("ya29.", 100),
    ];
    for (n, (prefix, len)) in samples.into_iter().enumerate() {
        let value = format!("{prefix}{}", body(n as u64, len));
        let found = detect(Place::Field("data"), &value).expect(prefix);
        assert_eq!(found.signal, Signal::Prefix(prefix));
        assert!(detect(Place::Header("X-Whatever"), &value).is_some());
    }
}

#[test]
fn a_jwt_is_a_token_wherever_it_is() {
    let value = jwt();
    assert_eq!(
        detect(Place::Field("payload"), &value).unwrap().signal,
        Signal::Jwt
    );
    assert_eq!(
        detect(Place::Header("Authorization"), &format!("Bearer {value}"))
            .unwrap()
            .signal,
        Signal::Jwt
    );
}

#[test]
fn a_named_field_needs_a_random_looking_value() {
    let value = body(3, 40);
    for name in [
        "access_token",
        "refresh_token",
        "id_token",
        "api_key",
        "apiKey",
        "clientSecret",
        "X-Auth-Token",
    ] {
        assert_eq!(
            detect(Place::Field(name), &value).map(|d| d.signal),
            Some(Signal::Named),
            "{name}"
        );
    }
    assert!(detect(Place::Header("X-Api-Key"), &value).is_some());
    assert!(detect(Place::Header("Authorization"), &format!("Bearer {value}")).is_some());
    assert!(detect(Place::Header("Authorization"), &format!("token {value}")).is_some());
    assert!(detect(Place::Cookie("sessionid"), &value).is_some());
    assert!(detect(Place::Cookie("__Secure-session"), &value).is_some());
    assert!(detect(Place::Field("github_access_token"), &value).is_some());
}

#[test]
fn the_same_value_under_an_ordinary_name_is_not_a_token() {
    let value = body(3, 40);
    for name in [
        "id", "name", "etag", "hash", "data", "value", "commit", "avatar",
    ] {
        assert_eq!(detect(Place::Field(name), &value), None, "{name}");
    }
    assert_eq!(detect(Place::Header("X-Request-Id"), &value), None);
    assert_eq!(detect(Place::Cookie("theme"), &value), None);
}

#[test]
fn non_credential_tokens_are_left_alone() {
    let value = body(4, 40);
    for name in [
        "csrf_token",
        "xsrf_token",
        "next_page_token",
        "continuation_token",
        "request_token",
        "token_type",
        "state",
        "nonce",
        "public_api_key",
    ] {
        assert_eq!(detect(Place::Field(name), &value), None, "{name}");
    }
    assert_eq!(detect(Place::Cookie("csrf_token"), &value), None);
}

#[test]
fn identifiers_numbers_words_and_addresses_are_never_tokens() {
    let samples = [
        "123e4567-e89b-12d3-a456-426614174000",
        "6b86b273-ff34-4fce-8c3f-1a3d9e2f3a10",
        "1234567890123456789012345",
        "undefined-undefined-undefined",
        "AuthorizationRequiredForThisResource",
        "access-denied-for-this-resource",
        "https://example.com/oauth/callback?x=1",
        "/usr/lib/x86_64-linux-gnu/libz.so.1",
        "2026-11-28T10:29:01.123456Z",
        "user.name@example.com",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
        "0000000000000000000000000000000a",
        "Bearer",
        "short1",
        "hunter2hunter2hunter2hunter2",
        "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "ghp_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx",
        "ghp_short",
        "a.b.c.d.e.f.g.h.i.j.k.l.m.n.o.p",
    ];
    for sample in samples {
        for place in [
            Place::Field("access_token"),
            Place::Field("data"),
            Place::Cookie("session"),
            Place::Header("X-Api-Key"),
        ] {
            assert_eq!(detect(place, sample), None, "{sample} at {place:?}");
        }
    }
}

#[test]
fn authorization_other_than_a_bearer_is_not_read() {
    let value = body(5, 40);
    assert_eq!(
        detect(Place::Header("Authorization"), &format!("Basic {value}")),
        None
    );
    assert_eq!(detect(Place::Header("Authorization"), &value), None);
}

#[test]
fn a_git_hash_is_not_a_token_under_an_ordinary_name_and_a_long_random_hex_is_one_under_a_token_name()
 {
    let sha = "e83c5163316f89bfbde7d9ab23ca2e25604af290";
    assert_eq!(detect(Place::Field("commit"), sha), None);
    assert!(detect(Place::Field("access_token"), sha).is_some());
}

#[test]
fn false_positive_budget_on_generated_ordinary_text() {
    // Ordinary words and ids joined at random must never trip the shape signals under an
    // ordinary name: the budget is zero of 2000.
    let mut hits = 0;
    for seed in 0..2000_u64 {
        let id = body(seed, 12 + usize::try_from(seed % 40).unwrap());
        if detect(Place::Field("id"), &id).is_some() || detect(Place::Cookie("lang"), &id).is_some()
        {
            hits += 1;
        }
    }
    assert_eq!(hits, 0);
}
