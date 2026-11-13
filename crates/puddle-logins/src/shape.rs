// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stand-ins that look like the token they stand for.
//!
//! A tool that is handed a stand-in may look at it: GitHub's tokens start with a type prefix
//! and secret scanners and some clients check it, Claude Code keeps the last characters of an
//! API key to remember that the user approved it. So a stand-in keeps the real token's prefix,
//! its length, its alphabet and, when a profile asks, its last characters; only the characters in
//! between are random. It is never a valid token: it carries no checksum, and it comes from the
//! system's random bytes, not from the real token.

use zeroize::Zeroizing;

use crate::profile::{Field, Keep, StandInKind};

/// The shortest and longest token a stand-in can copy the length of: the registry's own limits.
const MIN_LEN: usize = 16;
const MAX_LEN: usize = 256;

/// The fewest random characters a stand-in has, so it cannot be guessed or confused with another.
const MIN_RANDOM: usize = 8;

/// Why a stand-in could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ShapeError {
    /// The token is shorter than 16 or longer than its kind takes (256 for a shaped token, 8192
    /// for the other kinds), or has a character that is not
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
/// visible ASCII characters.
pub(crate) fn usable(real: &str) -> bool {
    (MIN_LEN..=MAX_LEN).contains(&real.len()) && real.bytes().all(|b| b.is_ascii_graphic())
}

/// Whether the token of `field` can be copied: [`usable`], and for a token whose fields are kept
/// the longer limit.
pub(crate) fn usable_for(field: &Field, real: &str) -> bool {
    match field.stand_in {
        StandInKind::Shaped => usable(real),
        StandInKind::Fields | StandInKind::Jwt | StandInKind::ShapedLong => long_copyable(real),
    }
}

/// Whether a token of fields or a JWT can be copied: the longer limit.
fn long_copyable(real: &str) -> bool {
    (MIN_LEN..=MAX_FIELDS_LEN).contains(&real.len()) && real.bytes().all(|b| b.is_ascii_graphic())
}

/// A stand-in for `real`, a token of the kind `field` describes: the same length, the prefix it
/// has among `field.prefixes`, the same alphabet and its last `field.keep_last` characters (when
/// the token is long enough to keep them and still leave random characters in between).
///
/// # Errors
/// [`ShapeError::Unusable`] for a token that cannot be copied; [`ShapeError::Random`] when the
/// system has no randomness.
pub fn stand_in_for(real: &str, field: &Field) -> Result<String, ShapeError> {
    let mut fill = |bytes: &mut [u8]| getrandom::fill(bytes).is_ok();
    match field.stand_in {
        StandInKind::Shaped | StandInKind::ShapedLong => stand_in_with(real, field, &mut fill),
        StandInKind::Fields => fields_stand_in_with(real, &mut fill),
        StandInKind::Jwt => match jwt_parts(real) {
            Some(_) => jwt_stand_in_with(real, &mut fill),
            // An opaque token (an Entra access token) is shaped.
            None => stand_in_with(real, field, &mut fill),
        },
    }
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

/// The `aud` claim of `real` when it is a JWT whose payload is a JSON object: the resource the token
/// is for, joined with spaces when the claim is a list. Read to tell which resource a token is for;
/// nothing is verified.
pub(crate) fn audience_of(real: &str) -> Option<String> {
    use base64::Engine as _;
    let (signed, _) = jwt_parts(real)?;
    let payload = signed.split_once('.')?.1;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    match claims.get("aud")? {
        serde_json::Value::String(one) => Some(one.clone()),
        serde_json::Value::Array(many) => Some(
            many.iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// Whether a refresh of `field`'s token needs a new stand-in: a session token's fields and a JWT's
/// payload change with each token (the expiry), and the client reads them, so the stand-in changes
/// with them; a token held in memory is one of many alive at once, so each has its own. Any other
/// token keeps its stand-in.
pub(crate) fn renews(real: &str, field: &Field) -> bool {
    if field.keep == Keep::Memory {
        return true;
    }
    match field.stand_in {
        StandInKind::Shaped | StandInKind::ShapedLong => false,
        StandInKind::Fields => true,
        StandInKind::Jwt => jwt_parts(real).is_some(),
    }
}

/// A stand-in for a JWT: its header and payload as they are and a random signature of the real
/// signature's length.
pub(crate) fn jwt_stand_in_with(
    real: &str,
    fill: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Result<String, ShapeError> {
    let (signed, signature) = jwt_parts(real).ok_or(ShapeError::Unusable)?;
    if !long_copyable(real) {
        return Err(ShapeError::Unusable);
    }
    let random = random_text(BASE64URL, signature.len(), fill)?;
    let stand_in = format!("{signed}.{random}");
    // Never hand the real token back as its own stand-in.
    if stand_in == real {
        return Err(ShapeError::Random);
    }
    Ok(stand_in)
}

/// The longest token of fields, a JWT or a long shaped token: a Copilot session token is a few
/// hundred characters, a Microsoft access token one to three thousand, a refresh token about
/// fifteen hundred. The registry's own limit.
const MAX_FIELDS_LEN: usize = 8192;

/// A stand-in for a token of `key=value;` fields and a MAC after the last colon: the fields as
/// they are, the MAC replaced by random characters of its alphabet and length. The client reads
/// the fields and checks no signature; the MAC is what the service checks, so it is the one part
/// that must never reach the workspace.
pub(crate) fn fields_stand_in_with(
    real: &str,
    fill: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Result<String, ShapeError> {
    let (fields, mac) = real.rsplit_once(':').ok_or(ShapeError::Unusable)?;
    if !long_copyable(real) || !fields.contains('=') || mac.len() < MIN_RANDOM {
        return Err(ShapeError::Unusable);
    }
    let random = Zeroizing::new(random_text(alphabet_of(mac), mac.len(), fill)?);
    let stand_in = format!("{fields}:{}", *random);
    if stand_in == real {
        return Err(ShapeError::Random);
    }
    Ok(stand_in)
}

pub(crate) fn stand_in_with(
    real: &str,
    field: &Field,
    fill: &mut dyn FnMut(&mut [u8]) -> bool,
) -> Result<String, ShapeError> {
    if !usable_for(field, real) {
        return Err(ShapeError::Unusable);
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
mod tests;
