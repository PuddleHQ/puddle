// SPDX-License-Identifier: AGPL-3.0-or-later
//! Telling a token from anything else, for a host Puddle has no profile for.
//!
//! A profile names the fields of a service's token answer. A host without one has nothing to
//! name, so this decides from the field's place (a header, a cookie, a JSON or form field), its
//! name and the value's shape. It is deliberately on the side of missing a token: a false
//! positive stops or rewrites something that was not a credential, a miss only leaves the
//! credential as it is today.
//!
//! Two signals are enough alone: a known credential prefix (`ghp_`, `sk-ant-`) and a JWT. A
//! well-known name (`access_token`, `X-Api-Key`, a session cookie) counts only together with a
//! value that is long, random-looking and not an identifier. Length plus entropy never counts
//! alone, and a UUID, a number, a URL or a word never counts at all.

use base64::Engine as _;

/// Where a value was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place<'a> {
    /// A request or response header, by its name.
    Header(&'a str),
    /// A cookie, by its name.
    Cookie(&'a str),
    /// A field of a JSON or form body, by its name (the last segment of its path).
    Field(&'a str),
}

/// Why a value counts as a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// It starts with a credential prefix a service documents.
    Prefix(&'static str),
    /// It has the structure of a JWT: three base64url segments, the first a JSON header with `alg`.
    Jwt,
    /// Its place names a credential and its value is random-looking.
    Named,
}

/// The verdict for a value that is a token; there is none for a value that is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detected {
    /// The reason, for the notice and the log (never the value).
    pub signal: Signal,
}

/// A prefix of a credential and the fewest characters after it. The services' own documentation
/// gives each; a prefix with a shorter rest is a placeholder or a word.
const PREFIXES: [(&str, usize); 16] = [
    ("github_pat_", 22),
    ("ghp_", 30),
    ("gho_", 30),
    ("ghu_", 30),
    ("ghs_", 30),
    ("ghr_", 30),
    ("sk-ant-", 20),
    ("sk-proj-", 20),
    ("sk_live_", 20),
    ("rk_live_", 20),
    ("xoxb-", 20),
    ("xoxp-", 20),
    ("glpat-", 20),
    ("npm_", 30),
    ("shpat_", 30),
    ("ya29.", 30),
];

/// The names of a credential, after the name is lowercased and `-`, `.` and spaces become `_`.
const CREDENTIAL_NAMES: [&str; 22] = [
    "access_token",
    "refresh_token",
    "id_token",
    "api_key",
    "apikey",
    "api_token",
    "auth_token",
    "bearer_token",
    "session_token",
    "client_secret",
    "secret_key",
    "secret_access_key",
    "private_token",
    "token",
    "jwt",
    "session",
    "sessionid",
    "session_id",
    "sid",
    "x_api_key",
    "x_auth_token",
    "x_access_token",
];

/// Name endings that are credentials too (`github_access_token`).
const CREDENTIAL_ENDINGS: [&str; 5] = [
    "_access_token",
    "_refresh_token",
    "_api_key",
    "_auth_token",
    "_session_token",
];

/// Words in a name that make it not a credential, whatever else it says: tokens that are not
/// secrets (CSRF, paging, request identifiers).
const NOT_CREDENTIALS: [&str; 14] = [
    "csrf",
    "xsrf",
    "nonce",
    "state",
    "challenge",
    "page",
    "next",
    "cursor",
    "continuation",
    "request",
    "trace",
    "device_code",
    "public",
    "type",
];

/// Header names that carry a credential besides `Authorization`.
const CREDENTIAL_HEADERS: [&str; 8] = [
    "x_api_key",
    "api_key",
    "x_auth_token",
    "x_access_token",
    "x_token",
    "private_token",
    "x_goog_api_key",
    "apikey",
];

/// The shortest and longest value that is ever a token here: the stand-in registry's own limits.
const MIN_LEN: usize = 16;
const MAX_LEN: usize = 8192;

/// Whether `value` is a token found at `place`, and why.
#[must_use]
pub fn detect(place: Place<'_>, value: &str) -> Option<Detected> {
    let value = value.trim();
    let value = match place {
        Place::Header(name) if normalize(name) == "authorization" => bearer_of(value)?,
        _ => value,
    };
    if !(MIN_LEN..=MAX_LEN).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    if is_uuid(value) {
        return None;
    }
    if is_jwt(value) {
        return Some(Detected {
            signal: Signal::Jwt,
        });
    }
    if let Some(prefix) = prefix_of(value) {
        return Some(Detected {
            signal: Signal::Prefix(prefix),
        });
    }
    (names_a_credential(place) && looks_random(value)).then_some(Detected {
        signal: Signal::Named,
    })
}

/// The value of an `Authorization` header without its scheme; `Basic` (a user and a password)
/// and an unknown scheme are not read.
fn bearer_of(value: &str) -> Option<&str> {
    let (scheme, rest) = value.split_once(' ')?;
    ["bearer", "token", "bot"]
        .iter()
        .any(|known| scheme.eq_ignore_ascii_case(known))
        .then_some(rest.trim())
}

fn normalize(name: &str) -> String {
    let mut snake = String::with_capacity(name.len() + 4);
    for c in name.trim().chars() {
        if c.is_ascii_uppercase() && !snake.is_empty() && !snake.ends_with('_') {
            snake.push('_');
        }
        snake.push(match c {
            '-' | '.' | ' ' => '_',
            c => c.to_ascii_lowercase(),
        });
    }
    let trimmed = snake
        .trim_start_matches("__secure_")
        .trim_start_matches("__host_");
    String::from(trimmed)
}

fn names_a_credential(place: Place<'_>) -> bool {
    let name = match place {
        Place::Header(name) => {
            let name = normalize(name);
            // Any value in `Authorization` was already reduced to its bearer part.
            return name == "authorization" || CREDENTIAL_HEADERS.contains(&name.as_str());
        }
        Place::Cookie(name) | Place::Field(name) => normalize(name),
    };
    if NOT_CREDENTIALS.iter().any(|word| name.contains(word)) {
        return false;
    }
    CREDENTIAL_NAMES.contains(&name.as_str())
        || CREDENTIAL_ENDINGS.iter().any(|end| name.ends_with(end))
        || name.ends_with("_session_token")
}

fn prefix_of(value: &str) -> Option<&'static str> {
    PREFIXES.iter().find_map(|(prefix, rest)| {
        let body = value.strip_prefix(prefix)?;
        (body.len() >= *rest && body.bytes().all(is_token_byte) && shannon(body) >= 2.5)
            .then_some(*prefix)
    })
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// Whether `value` is a UUID (8-4-4-4-12 hex digits): an identifier, never a secret.
fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Three base64url segments whose first decodes to a JSON object with `alg`.
fn is_jwt(value: &str) -> bool {
    let mut parts = value.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let base64url = |text: &str| {
        !text.is_empty()
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if !(base64url(header) && base64url(payload) && base64url(signature)) || signature.len() < 8 {
        return false;
    }
    let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(header) else {
        return false;
    };
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .is_ok_and(|json| json.get("alg").is_some_and(serde_json::Value::is_string))
}

/// Long enough, one visible alphabet, digits and letters both, many different characters and
/// high entropy: the value of a token, not of a word, a number, a path, an address or a timestamp.
fn looks_random(value: &str) -> bool {
    let bytes = value.as_bytes();
    if !bytes.iter().copied().all(is_token_byte) || value.starts_with('/') || value.contains("//") {
        return false;
    }
    let digits = bytes.iter().filter(|b| b.is_ascii_digit()).count();
    let letters = bytes.iter().filter(|b| b.is_ascii_alphabetic()).count();
    if digits == 0 || letters == 0 {
        return false;
    }
    let mut seen = [false; 256];
    for &b in bytes {
        if let Some(slot) = seen.get_mut(usize::from(b)) {
            *slot = true;
        }
    }
    seen.iter().filter(|s| **s).count() >= 10 && shannon(value) >= 3.2
}

/// The Shannon entropy of the bytes of `text`, in bits per character.
#[expect(clippy::cast_precision_loss, reason = "a text of at most 8192 bytes")]
fn shannon(text: &str) -> f64 {
    let mut counts = [0_u32; 256];
    for &b in text.as_bytes() {
        if let Some(count) = counts.get_mut(usize::from(b)) {
            *count += 1;
        }
    }
    let total = text.len() as f64;
    if total == 0.0 {
        return 0.0;
    }
    counts
        .iter()
        .filter(|c| **c > 0)
        .map(|&c| {
            let p = f64::from(c) / total;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests;
