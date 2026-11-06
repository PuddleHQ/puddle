// SPDX-License-Identifier: GPL-3.0-or-later
//! Stand-ins that look like the token they stand for.
//!
//! A tool that is handed a stand-in may look at it: GitHub's tokens start with a type prefix
//! and secret scanners and some clients check it, Claude Code keeps the last characters of an
//! API key to remember that the user approved it. So a stand-in keeps the real token's prefix,
//! its length, its alphabet and, when a profile asks, its last characters; only the characters in
//! between are random. It is never a valid token: it carries no checksum, and it comes from the
//! system's random bytes, not from the real token.

use zeroize::Zeroizing;

use crate::profile::{Field, Generator};

/// The shortest and longest token a stand-in can copy the length of, for a shaped token.
const MIN_LEN: usize = 16;
const MAX_LEN: usize = 256;
/// The longest a field made by [`Generator::Jwt`] may be: a JWT is longer than other tokens. It is
/// the registry's own limit.
const MAX_JWT_LEN: usize = 1024;

/// The fewest random characters a stand-in has, so it cannot be guessed or confused with another.
const MIN_RANDOM: usize = 8;

/// Why a stand-in could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ShapeError {
    /// The token is shorter than 16 or longer than 256 characters, or has a character that is not
    /// visible ASCII (a space, a control character, anything outside ASCII): a header could not
    /// carry it as it is, or the registry would not accept its stand-in.
    #[error("the token has a length or characters a stand-in cannot copy")]
    Unusable,
    /// The operating system has no randomness to give.
    #[error("the operating system has no randomness to give")]
    Random,
}

const HEX: &[u8] = b"0123456789abcdef";
const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const BASE64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The smallest alphabet that holds `body`; any other characters give the common one.
fn alphabet_of(body: &str) -> &'static [u8] {
    if body.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        HEX
    } else if body.bytes().all(|b| b.is_ascii_alphanumeric()) {
        BASE62
    } else if body
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        BASE64URL
    } else {
        BASE62
    }
}

/// `count` characters of `alphabet`, uniformly, from `fill`'s random bytes (bytes that would make
/// the choice uneven are dropped, not folded).
fn random_text(
    alphabet: &[u8],
    count: usize,
    fill: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Result<String, ShapeError> {
    // The largest multiple of the alphabet's size that fits in a byte.
    let limit = (256 / alphabet.len()) * alphabet.len();
    let mut text = String::with_capacity(count);
    let mut bytes = [0_u8; 64];
    while text.len() < count {
        if !fill(&mut bytes) {
            return Err(ShapeError::Random);
        }
        for byte in bytes {
            if usize::from(byte) >= limit {
                continue;
            }
            if let Some(&c) = alphabet.get(usize::from(byte) % alphabet.len()) {
                text.push(char::from(c));
            }
            if text.len() == count {
                break;
            }
        }
    }
    Ok(text)
}

/// Whether a token can be copied by a stand-in and can be swapped back into a header: 16 to 256
/// visible ASCII characters (up to 1024 for a field of [`Generator::Jwt`]).
pub(crate) fn usable(real: &str, field: &Field) -> bool {
    let most = match field.generator {
        Generator::Shaped => MAX_LEN,
        Generator::Jwt => MAX_JWT_LEN,
    };
    (MIN_LEN..=most).contains(&real.len()) && real.bytes().all(|b| b.is_ascii_graphic())
}

/// The signed part of a JWT (`header.payload`) and its signature, when `real` is one: three
/// segments of base64url, the first two the encoding of a JSON object (`ey` is `{"`), and a
/// signature with enough characters to replace them with random ones.
fn jwt_parts(real: &str) -> Option<(&str, &str)> {
    let (signed, signature) = real.rsplit_once('.')?;
    let (header, payload) = signed.split_once('.')?;
    let base64url = |text: &str| text.bytes().all(|b| BASE64URL.contains(&b));
    let is_jwt = header.starts_with("ey")
        && payload.starts_with("ey")
        && base64url(header)
        && base64url(payload)
        && base64url(signature)
        && signature.len() >= MIN_RANDOM;
    is_jwt.then_some((signed, signature))
}

/// Whether a refresh of `field`'s token needs a new stand-in: a JWT's payload changes with each
/// token (its expiry), and the tool reads it, so the stand-in must change with it. Any other token
/// keeps its stand-in.
pub(crate) fn renews(real: &str, field: &Field) -> bool {
    field.generator == Generator::Jwt && jwt_parts(real).is_some()
}

/// A stand-in for `real`, a token of the kind `field` describes. A field of [`Generator::Jwt`] and a
/// token that is a JWT: its header and payload with a random signature of the same length. Any other:
/// the same length, the prefix it
/// has among `field.prefixes`, the same alphabet and its last `field.keep_last` characters (when
/// the token is long enough to keep them and still leave random characters in between).
///
/// # Errors
/// [`ShapeError::Unusable`] for a token that cannot be copied; [`ShapeError::Random`] when the
/// system has no randomness.
pub fn stand_in_for(real: &str, field: &Field) -> Result<String, ShapeError> {
    stand_in_with(real, field, &mut |bytes| getrandom::fill(bytes).is_ok())
}

pub(crate) fn stand_in_with(
    real: &str,
    field: &Field,
    fill: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Result<String, ShapeError> {
    if !usable(real, field) {
        return Err(ShapeError::Unusable);
    }
    if field.generator == Generator::Jwt {
        if let Some((signed, signature)) = jwt_parts(real) {
            let random = random_text(BASE64URL, signature.len(), fill)?;
            let stand_in = format!("{signed}.{random}");
            // Not a proper check of the signature's randomness, only that it is not the real one.
            return if stand_in == real {
                Err(ShapeError::Random)
            } else {
                Ok(stand_in)
            };
        }
    }
    let prefix = field
        .prefixes
        .iter()
        .copied()
        .find(|prefix| real.starts_with(prefix))
        .unwrap_or_default();
    // `real` is visible ASCII, so every split below is on a character boundary.
    let body = real.strip_prefix(prefix).unwrap_or(real);
    let keep = if body.len() >= field.keep_last + MIN_RANDOM {
        field.keep_last
    } else {
        0
    };
    let (middle, tail) = body
        .split_at_checked(body.len() - keep)
        .unwrap_or((body, ""));
    let random = Zeroizing::new(random_text(alphabet_of(middle), middle.len(), fill)?);
    let stand_in = format!("{prefix}{}{tail}", *random);
    if stand_in == real {
        // 62^-8 at best: still never hand the real token back as its own stand-in.
        return Err(ShapeError::Random);
    }
    Ok(stand_in)
}

#[cfg(test)]
mod tests {
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
            generator: Generator::Jwt,
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
        assert_eq!(
            stand_in_with(&real, &jwt_field(), &mut fill),
            Err(ShapeError::Random)
        );
        assert_eq!(
            stand_in_with(&real, &jwt_field(), &mut |_| false),
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
    fn a_jwt_field_takes_up_to_a_thousand_and_twenty_four_characters_and_a_shaped_one_256() {
        let long = jwt(r#"{"exp":1}"#, 1024 - jwt(r#"{"exp":1}"#, 0).len());
        assert_eq!(long.len(), 1024);
        assert_eq!(stand_in_for(&long, &jwt_field()).unwrap().len(), 1024);
        assert_eq!(
            stand_in_for(&format!("{long}x"), &jwt_field()),
            Err(ShapeError::Unusable)
        );
        assert_eq!(
            stand_in_for(&long, github_access()),
            Err(ShapeError::Unusable)
        );
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
}
