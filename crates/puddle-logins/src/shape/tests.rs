// SPDX-License-Identifier: GPL-3.0-or-later
use proptest::prelude::*;

use super::*;
use crate::profile::{CLAUDE, GITHUB};

fn github_access() -> &'static Field {
    &GITHUB.endpoints[0].fields[0]
}

fn claude_key() -> &'static Field {
    &CLAUDE
        .endpoints
        .iter()
        .find(|e| e.host == "api.anthropic.com")
        .unwrap()
        .fields[0]
}

fn counting() -> impl FnMut(&mut [u8]) -> bool {
    let mut n = 0_u8;
    move |bytes| {
        for b in bytes {
            *b = n;
            n = n.wrapping_add(37);
        }
        true
    }
}

#[test]
fn a_github_token_keeps_its_prefix_its_length_and_its_alphabet() {
    let real = "gho_CANARY-fake-github-token-0123456789ab";
    let stand_in = stand_in_for(real, github_access()).unwrap();
    assert_eq!(stand_in.len(), real.len());
    assert!(stand_in.starts_with("gho_"));
    assert_ne!(stand_in, real);
    assert!(
        stand_in
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    );
    assert!(
        !stand_in.contains(&real[4..12]),
        "nothing of the middle is kept"
    );
}

#[test]
fn a_claude_api_key_keeps_its_last_twenty_characters() {
    let real = format!(
        "sk-ant-api03-{}{}",
        "A".repeat(60),
        "wXyZ-_0123456789abcdef"
    );
    assert!(real.len() > 13 + 20 + 8);
    let stand_in = stand_in_for(&real, claude_key()).unwrap();
    assert_eq!(stand_in.len(), real.len());
    assert!(stand_in.starts_with("sk-ant-api03-"));
    assert!(stand_in.ends_with(&real[real.len() - 20..]));
    assert_ne!(stand_in, real);
    assert!(!stand_in.contains(&"A".repeat(10)));
}

#[test]
fn a_token_too_short_to_keep_its_end_and_stay_random_keeps_nothing_of_it() {
    let real = "sk-ant-api03-ABCDEFGHIJ1234567";
    let stand_in = stand_in_for(real, claude_key()).unwrap();
    assert_eq!(stand_in.len(), real.len());
    assert!(!stand_in.ends_with("1234567"));
}

#[test]
fn the_alphabet_follows_the_token() {
    assert_eq!(alphabet_of("0123abcdef"), HEX);
    assert_eq!(alphabet_of("0123abcDEF"), BASE62);
    assert_eq!(alphabet_of("ab-cd_ef"), BASE64URL);
    assert_eq!(alphabet_of("ab.cd~ef"), BASE62);
    let hex = stand_in_for(&"0123456789abcdef".repeat(2), github_access()).unwrap();
    assert!(hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
}

#[test]
fn an_unknown_prefix_is_not_kept() {
    let stand_in =
        stand_in_for("ghx_CANARY-fake-github-token-0123456789ab", github_access()).unwrap();
    assert!(!stand_in.starts_with("ghx_"));
}

#[test]
fn tokens_a_stand_in_cannot_copy_are_refused() {
    for real in [
        "",
        "short",
        "gho_with a space in the middle 123456",
        "gho_tab\there1234567890123456",
        "gho_non-ascii-é-0123456789012345",
        &"x".repeat(257),
    ] {
        assert_eq!(
            stand_in_for(real, github_access()),
            Err(ShapeError::Unusable),
            "{real:?}"
        );
    }
    assert!(stand_in_for(&"x".repeat(256), github_access()).is_ok());
    assert!(stand_in_for(&"x".repeat(16), github_access()).is_ok());
}

const SESSION_MAC: &str = "9f8e7d6c5b4a39281706f5e4d3c2b1a09f8e7d6c5b4a39281706f5e4d3c2b1a0";

fn session_token() -> String {
    format!(
        "tid=7a1b;exp=1790000000;sku=free_limited_copilot;proxy-ep=proxy.individual.githubcopilot.com;st=dotcom;chat=1:{SESSION_MAC}"
    )
}

fn copilot_session() -> &'static Field {
    GITHUB
        .fields()
        .find(|(_, f)| f.role == crate::profile::Role::Copilot)
        .map(|(_, f)| f)
        .unwrap()
}

#[test]
fn a_session_token_keeps_its_fields_and_only_its_mac_is_random() {
    let real = session_token();
    let stand_in = stand_in_for(&real, copilot_session()).unwrap();
    let (fields, mac) = stand_in.split_once(':').unwrap();
    assert_eq!(Some(fields), real.split_once(':').map(|(f, _)| f));
    assert_eq!(mac.len(), SESSION_MAC.len());
    assert_ne!(mac, SESSION_MAC);
    assert!(mac.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    assert!(!stand_in.contains(&SESSION_MAC[..12]));
    // A client that reads the fields as the extension does finds the same ones.
    let fields_read = |t: &str| -> Vec<(String, String)> {
        t.split(':')
            .next()
            .unwrap()
            .split(';')
            .map(|f| {
                let (k, v) = f.split_once('=').unwrap();
                (k.to_owned(), v.to_owned())
            })
            .collect()
    };
    assert_eq!(fields_read(&stand_in), fields_read(&real));
    assert_ne!(stand_in, stand_in_for(&real, copilot_session()).unwrap());
}

#[test]
fn a_session_token_whose_fields_hold_a_colon_keeps_them_and_replaces_only_the_mac_after_the_last() {
    // A field value may itself hold a colon; the MAC is what follows the last one.
    let real = format!("tid=7a1b;exp=1790000000;8kp=1:0;proxy-ep=proxy.example:443:{SESSION_MAC}");
    let stand_in = stand_in_for(&real, copilot_session()).unwrap();
    let (fields, mac) = stand_in.rsplit_once(':').unwrap();
    assert_eq!(
        fields,
        "tid=7a1b;exp=1790000000;8kp=1:0;proxy-ep=proxy.example:443"
    );
    assert_eq!(mac.len(), SESSION_MAC.len());
    assert_ne!(mac, SESSION_MAC);
    assert!(!stand_in.contains(&SESSION_MAC[..12]));
}

#[test]
fn a_session_token_may_be_longer_than_an_ordinary_token() {
    let long = format!("tid=1;pad={}:{SESSION_MAC}", "x".repeat(700));
    assert!(stand_in_for(&long, copilot_session()).is_ok());
    assert!(usable_for(copilot_session(), &long));
    assert!(!usable_for(github_access(), &long));
    let too_long = format!("tid=1;pad={}:{SESSION_MAC}", "x".repeat(8200));
    assert_eq!(
        stand_in_for(&too_long, copilot_session()),
        Err(ShapeError::Unusable)
    );
}

#[test]
fn a_session_token_without_fields_or_mac_is_refused() {
    for real in [
        "no-colon-at-all-in-this-token-0123456789".to_owned(),
        format!("no-equals-sign-in-fields:{SESSION_MAC}"),
        "tid=1;exp=2;sku=x;proxy-ep=y;st=dotcom:abc".to_owned(),
        format!("tid=1 ;exp=2:{SESSION_MAC}"),
    ] {
        assert_eq!(
            stand_in_for(&real, copilot_session()),
            Err(ShapeError::Unusable),
            "{real:?}"
        );
    }
}

#[test]
fn a_session_token_stand_in_needs_randomness_and_is_never_the_real_token() {
    let real = session_token();
    assert_eq!(
        fields_stand_in_with(&real, &mut |_| false),
        Err(ShapeError::Random)
    );
    let mac = SESSION_MAC.as_bytes().to_vec();
    let mut at = 0;
    let mut fill = |bytes: &mut [u8]| {
        for b in bytes {
            let want = mac[at % mac.len()];
            *b = u8::try_from(HEX.iter().position(|c| *c == want).unwrap()).unwrap();
            at += 1;
        }
        true
    };
    assert_eq!(
        fields_stand_in_with(&real, &mut fill),
        Err(ShapeError::Random)
    );
}

#[test]
fn no_randomness_is_an_error_not_a_weak_stand_in() {
    let real = "gho_CANARY-fake-github-token-0123456789ab";
    assert_eq!(
        stand_in_with(real, github_access(), &mut |_| false),
        Err(ShapeError::Random)
    );
}

#[test]
fn a_stand_in_equal_to_the_real_token_is_refused() {
    // A source of bytes that spells the token itself.
    let real = "gho_abcdefghijklmnopqrstuvwxyz0123456789";
    let body = real.as_bytes()[4..].to_vec();
    let mut at = 0;
    let mut fill = |bytes: &mut [u8]| {
        for b in bytes {
            // BASE62 index of the wanted character.
            let want = body[at % body.len()];
            *b = u8::try_from(BASE62.iter().position(|c| *c == want).unwrap()).unwrap();
            at += 1;
        }
        true
    };
    assert_eq!(
        stand_in_with(real, github_access(), &mut fill),
        Err(ShapeError::Random)
    );
}

#[test]
fn bytes_that_would_make_the_choice_uneven_are_dropped() {
    // 62 does not divide 256: bytes 248..=255 would favour the first eight characters.
    let mut calls = 0;
    let mut fill = |bytes: &mut [u8]| {
        calls += 1;
        bytes.fill(250);
        if calls > 1 {
            bytes.fill(1);
        }
        true
    };
    let text = random_text(BASE62, 5, &mut fill).unwrap();
    assert_eq!(text, "11111");
    assert!(calls >= 2);
}

#[test]
fn the_same_source_gives_the_same_stand_in() {
    let real = "gho_CANARY-fake-github-token-0123456789ab";
    let a = stand_in_with(real, github_access(), &mut counting()).unwrap();
    let b = stand_in_with(real, github_access(), &mut counting()).unwrap();
    assert_eq!(a, b);
}

fn jwt_field() -> Field {
    Field {
        stand_in: StandInKind::Jwt,
        prefixes: &[],
        ..*github_access()
    }
}

/// A JWT of `signature_len` signature characters, header `{"alg":"RS256"}`, claims `claims`.
fn jwt(claims: &str, signature_len: usize) -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        URL_SAFE_NO_PAD.encode(claims),
        "S".repeat(signature_len)
    )
}

#[test]
fn a_jwt_keeps_its_header_and_claims_and_gets_a_random_signature_of_the_same_length() {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let real = jwt(r#"{"sub":"me","tid":"tenant-a","exp":1893456000}"#, 342);
    let stand_in = stand_in_for(&real, &jwt_field()).unwrap();
    assert_eq!(stand_in.len(), real.len());
    let (real_parts, parts): (Vec<&str>, Vec<&str>) =
        (real.split('.').collect(), stand_in.split('.').collect());
    assert_eq!(parts.len(), 3);
    assert_eq!((parts[0], parts[1]), (real_parts[0], real_parts[1]));
    // What a tool that reads the claims sees.
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["exp"], 1_893_456_000);
    assert_eq!(claims["tid"], "tenant-a");
    let header: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "RS256");
    // The signature is new, and never the real one.
    assert_ne!(parts[2], real_parts[2]);
    assert_eq!(parts[2].len(), 342);
    assert!(parts[2].bytes().all(|b| BASE64URL.contains(&b)));
    assert!(!stand_in.contains("SSSSSSSS"));
}

#[test]
fn two_stand_ins_of_one_jwt_differ_only_in_the_signature() {
    let real = jwt(r#"{"exp":1893456000}"#, 43);
    let a = stand_in_for(&real, &jwt_field()).unwrap();
    let b = stand_in_for(&real, &jwt_field()).unwrap();
    assert_ne!(a, b);
    assert_eq!(a.rsplit_once('.').unwrap().0, b.rsplit_once('.').unwrap().0);
}

#[test]
fn a_stand_in_of_a_jwt_equal_to_the_real_one_is_refused() {
    let real = jwt(r#"{"exp":1893456000}"#, 8);
    let signature = real.rsplit_once('.').unwrap().1.as_bytes().to_vec();
    let mut at = 0;
    let mut fill = |bytes: &mut [u8]| {
        for b in bytes {
            let want = signature[at % signature.len()];
            *b = u8::try_from(BASE64URL.iter().position(|c| *c == want).unwrap()).unwrap();
            at += 1;
        }
        true
    };
    assert_eq!(jwt_stand_in_with(&real, &mut fill), Err(ShapeError::Random));
    assert_eq!(
        jwt_stand_in_with(&real, &mut |_| false),
        Err(ShapeError::Random)
    );
}

#[test]
fn a_token_that_is_not_a_jwt_is_shaped_by_a_jwt_field() {
    let field = jwt_field();
    let ey = |text: &str| format!("ey{text}");
    for opaque in [
        "EwBIA8l6BAAU-opaque-token-0123456789abcdef".to_owned(),
        // Only two segments, an empty signature (`alg` none), a signature that is too short
        // to replace, a dot in a fourth place, text outside base64url.
        format!("{}.{}", ey("Jhbxxxxxx"), ey("Jzdxxxxxxxx")),
        format!("{}.{}.", ey("Jhbxxxxxx"), ey("Jzdxxxxxxxx")),
        format!("{}.{}.short", ey("Jhbxxxxxx"), ey("Jzdxxxxxxxx")),
        format!(
            "{}.{}.abcdefgh.abcdefgh",
            ey("Jhbxxxxxx"),
            ey("Jzdxxxxxxxx")
        ),
        format!("{}.{}.abcdef+ghi", ey("Jhbxxxxxx"), ey("Jzdxxxxxxxx")),
        // A first segment that is not the encoding of an object.
        format!("Zm9vYmFy.{}.abcdefghijkl", ey("Jzdxxxxxxxx")),
        format!("{}.Zm9vYmFy.abcdefghijkl", ey("Jhbxxxxxx")),
    ] {
        assert!(!renews(&opaque, &field), "{opaque}");
        let stand_in = stand_in_for(&opaque, &field).unwrap();
        assert_eq!(stand_in.len(), opaque.len());
        assert_ne!(stand_in, opaque);
    }
    let real = jwt(r#"{"exp":1}"#, 43);
    assert!(renews(&real, &field));
    // A shaped field never renews, whatever the token is.
    assert!(!renews(&real, github_access()));
}

#[test]
fn a_jwt_field_takes_up_to_the_registry_limit_and_a_shaped_one_256() {
    let long = jwt(r#"{"exp":1}"#, 8192 - jwt(r#"{"exp":1}"#, 0).len());
    assert_eq!(long.len(), 8192);
    assert_eq!(stand_in_for(&long, &jwt_field()).unwrap().len(), 8192);
    assert_eq!(
        stand_in_for(&format!("{long}x"), &jwt_field()),
        Err(ShapeError::Unusable)
    );
    assert_eq!(
        stand_in_for(&long, github_access()),
        Err(ShapeError::Unusable)
    );
}

#[test]
fn a_long_shaped_field_copies_a_token_of_fifteen_hundred_characters() {
    let field = Field {
        stand_in: StandInKind::ShapedLong,
        prefixes: &["1."],
        ..*github_access()
    };
    let real = format!("1.A{}", "r".repeat(1500));
    let stand_in = stand_in_for(&real, &field).unwrap();
    assert_eq!(stand_in.len(), real.len());
    assert!(stand_in.starts_with("1."));
    assert_ne!(stand_in, real);
    assert!(!renews(&real, &field));
    // A plain shaped field still stops at 256.
    assert_eq!(
        stand_in_for(&real, github_access()),
        Err(ShapeError::Unusable)
    );
    assert_eq!(
        stand_in_for(&"r".repeat(8193), &field),
        Err(ShapeError::Unusable)
    );
}

#[test]
fn the_audience_of_a_jwt_is_its_aud_claim_and_nothing_else_has_one() {
    assert_eq!(
        audience_of(&jwt(r#"{"aud":"https://graph.microsoft.com","exp":1}"#, 43)).as_deref(),
        Some("https://graph.microsoft.com")
    );
    assert_eq!(
        audience_of(&jwt(r#"{"aud":["a","b"],"exp":1}"#, 43)).as_deref(),
        Some("a b")
    );
    assert_eq!(audience_of(&jwt(r#"{"exp":1}"#, 43)), None);
    assert_eq!(audience_of(&jwt(r#"{"aud":7}"#, 43)), None);
    assert_eq!(audience_of("EwB-not-a-jwt-0123456789abcdef"), None);
}

proptest! {
    #[test]
    fn a_stand_in_has_the_shape_of_its_token(
        prefix in prop::sample::select(vec!["gho_", "ghu_", ""]),
        body in "[A-Za-z0-9]{16,120}",
    ) {
        let real = format!("{prefix}{body}");
        let stand_in = stand_in_for(&real, github_access()).unwrap();
        prop_assert_eq!(stand_in.len(), real.len());
        prop_assert_ne!(&stand_in, &real);
        prop_assert!(stand_in.starts_with(prefix));
        prop_assert!(stand_in.bytes().all(|b| b.is_ascii_graphic()));
    }

    #[test]
    fn any_text_either_gives_a_graphic_stand_in_of_its_length_or_is_refused(
        real in prop_oneof!["[!-~]{0,300}", "\\PC{0,300}"]
    ) {
        match stand_in_for(&real, claude_key()) {
            Ok(stand_in) => {
                prop_assert_eq!(stand_in.len(), real.len());
                prop_assert!(stand_in.bytes().all(|b| b.is_ascii_graphic()));
            }
            // Too short, too long or not visible ASCII; the system's randomness does not fail.
            Err(ShapeError::Unusable | ShapeError::Random) => {}
        }
    }
}
