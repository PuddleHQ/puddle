// SPDX-License-Identifier: AGPL-3.0-or-later
//! Text that may carry a secret, with the secret cut out.
//!
//! Puddle's code never logs a secret, so this is the second line: everything that leaves the
//! machine in a log bundle passes through it first. It errs towards cutting too much.

use serde_json::Value;

/// What stands where a secret was.
pub const REDACTED: &str = "[redacted]";

/// Words in a key that say its value is a secret.
const SECRET_KEYS: [&str; 10] = [
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "private_key",
    "passphrase",
];

/// Headers whose value is the rest of the line.
const LINE_KEYS: [&str; 3] = ["authorization", "cookie", "set-cookie"];

/// Prefixes of well-known token shapes.
const TOKEN_PREFIXES: [&str; 12] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-",
    "xoxb-",
    "xoxp-",
    "npm_",
    "AKIA",
];

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '/' | '=')
}

fn key_is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|k| key.contains(k))
}

fn key_is_line(key: &str) -> bool {
    let key = key
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .to_ascii_lowercase();
    LINE_KEYS.contains(&key.as_str())
}

/// A word that is a token by its shape: a known prefix or a JSON web token.
fn word_is_token(word: &str) -> bool {
    if TOKEN_PREFIXES.iter().any(|p| word.starts_with(p)) && word.len() > 12 {
        return true;
    }
    word.starts_with("eyJ") && word.matches('.').count() == 2
}

/// `text` with every word of a known token shape replaced.
fn redact_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word_is_token(word) {
            out.push_str(REDACTED);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if is_word_char(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The address part of `token` made safe: no user or password before an `@`, no query string.
fn redact_url(token: &str) -> String {
    let Some((scheme, rest)) = token.split_once("://") else {
        return token.to_owned();
    };
    let (authority, tail) = rest
        .find(['/', '?', '#'])
        .map_or((rest, ""), |i| rest.split_at(i));
    let authority = authority.rsplit_once('@').map_or_else(
        || authority.to_owned(),
        |(_, host)| format!("{REDACTED}@{host}"),
    );
    let tail = tail
        .split_once('?')
        .map_or_else(|| tail.to_owned(), |(path, _)| format!("{path}?{REDACTED}"));
    format!("{scheme}://{authority}{tail}")
}

/// The characters a value may end in that belong to the text around it, not to the value.
fn trailing(value: &str) -> &str {
    let end = value
        .trim_end_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '}' | ']'))
        .len();
    value.get(end..).unwrap_or("")
}

/// One whitespace-free token, or the text before the value when the value is the next token.
enum Next {
    Plain,
    /// The next token is a secret value.
    OneToken,
    /// The rest of the line is.
    RestOfLine,
}

fn redact_token(token: &str, state: &mut Next) -> String {
    match std::mem::replace(state, Next::Plain) {
        Next::RestOfLine => {
            *state = Next::RestOfLine;
            return REDACTED.to_owned();
        }
        Next::OneToken => return format!("{REDACTED}{}", trailing(token)),
        Next::Plain => {}
    }
    if matches!(token, "Bearer" | "Basic" | "Token") {
        *state = Next::OneToken;
        return token.to_owned();
    }
    if let Some(i) = token.find(['=', ':']) {
        let (key, rest) = token.split_at(i);
        let value = rest.get(1..).unwrap_or("");
        // `://` in a URL is not a key and a value.
        if !rest.starts_with("://") && (key_is_secret(key) || key_is_line(key)) {
            let value = value.trim_start_matches(['"', '\'']);
            if value.is_empty() {
                *state = if key_is_line(key) {
                    Next::RestOfLine
                } else {
                    Next::OneToken
                };
                return token.to_owned();
            }
            let sep = rest.get(..1).unwrap_or("");
            if key_is_line(key) {
                *state = Next::RestOfLine;
            }
            return format!("{key}{sep}{REDACTED}{}", trailing(value));
        }
    }
    redact_words(&redact_url(token))
}

/// `text` with its secrets cut out: the value of a key named like a secret (`token=...`,
/// `"password": "..."`, `Authorization: ...`), a `Bearer` or `Basic` credential, a word shaped like
/// a known token, the user and password of a URL and a URL's query string.
#[must_use]
pub fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, line) in text.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut state = Next::Plain;
        let mut rest = line;
        while !rest.is_empty() {
            let space_end = rest
                .find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len());
            let (space, after) = rest.split_at(space_end);
            out.push_str(space);
            let token_end = after.find(char::is_whitespace).unwrap_or(after.len());
            let (token, tail) = after.split_at(token_end);
            out.push_str(&redact_token(token, &mut state));
            rest = tail;
        }
    }
    out
}

/// `value` with every string value under a key named like a secret replaced and every other string
/// passed through [`redact`].
pub fn redact_json(value: &mut Value) {
    match value {
        Value::String(s) => *s = redact(s),
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key_is_secret(key) && !item.is_null() {
                    *item = Value::String(REDACTED.to_owned());
                } else {
                    redact_json(item);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
