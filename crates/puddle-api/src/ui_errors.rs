// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors the app's pages hit that no code expected. A page reports each one
//! (`POST /api/ui-errors`); the server scrubs it, writes it to the host's log and keeps the most
//! recent for Settings to list (`GET /api/ui-errors`). A page caught in a loop of errors cannot
//! flood the log: each user's reports are rate-limited.
//!
//! The scrubbing is a safety net, not a proof: it takes out the user info, query and fragment of
//! every URL, the word after an authorization scheme, the value of a `name=value` or
//! `name: value` whose name says secret, and any word shaped like a token (a known prefix, a JWT,
//! or a long run of letters and digits). A secret in plain words, or a short one in no such
//! shape, gets through.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Mutex, PoisonError};

use puddle_store::UserId;

use puddle_services::wire::{UiError, UiErrorList, UiErrorReport};

/// The most errors kept, for all users together; the oldest make room.
pub(crate) const KEEP: usize = 50;
/// The most reports a user's pages may send in one [`WINDOW_MS`].
pub(crate) const PER_WINDOW: usize = 10;
/// The window the rate is counted over.
pub(crate) const WINDOW_MS: u64 = 60_000;

const MESSAGE_CHARS: usize = 1000;
const MESSAGE_LINES: usize = 5;
const STACK_CHARS: usize = 4000;
const STACK_LINES: usize = 40;
pub(crate) const ROUTE_CHARS: usize = 200;
const BUILD_CHARS: usize = 64;

/// What replaces a secret.
const REDACTED: &str = "[redacted]";
/// What ends a cut text.
const CUT: char = '…';

/// What became of a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Kept, as scrubbed; log it.
    Kept(UiError),
    /// Over the rate. `first` is the first refusal since the user's last report was taken: log
    /// that one, not the rest.
    Refused { retry_after_s: u64, first: bool },
}

/// The errors kept and each user's recent reports.
#[derive(Debug, Default)]
pub(crate) struct UiErrors {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    kept: VecDeque<(UserId, UiError)>,
    /// Per user, when each report of the current window was taken.
    taken: HashMap<UserId, VecDeque<u64>>,
    dropped: HashMap<UserId, u64>,
    /// The users refused since their last report was taken.
    refusing: HashSet<UserId>,
}

impl UiErrors {
    /// Takes `report` from `user`'s page at `now` (epoch ms), unless the user is over the rate.
    pub(crate) fn report(&self, user: UserId, report: &UiErrorReport, now: u64) -> Outcome {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let inner = &mut *guard;
        // Forget what fell out of the window, and the users with nothing left in it.
        inner.taken.retain(|_, taken| {
            while taken
                .front()
                .is_some_and(|at| now.saturating_sub(*at) >= WINDOW_MS)
            {
                taken.pop_front();
            }
            !taken.is_empty()
        });
        let taken = inner.taken.entry(user).or_default();
        if taken.len() >= PER_WINDOW {
            let oldest = taken.front().copied().unwrap_or(now);
            *inner.dropped.entry(user).or_default() += 1;
            return Outcome::Refused {
                retry_after_s: (oldest + WINDOW_MS)
                    .saturating_sub(now)
                    .div_ceil(1000)
                    .max(1),
                first: inner.refusing.insert(user),
            };
        }
        taken.push_back(now);
        inner.refusing.remove(&user);
        let error = scrubbed(report, now);
        if inner.kept.len() >= KEEP {
            inner.kept.pop_front();
        }
        inner.kept.push_back((user, error.clone()));
        Outcome::Kept(error)
    }

    /// `user`'s errors, newest first, and how many of their reports were refused.
    pub(crate) fn list(&self, user: UserId) -> UiErrorList {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        UiErrorList {
            errors: inner
                .kept
                .iter()
                .rev()
                .filter(|(owner, _)| *owner == user)
                .map(|(_, error)| error.clone())
                .collect(),
            dropped: inner.dropped.get(&user).copied().unwrap_or(0),
        }
    }
}

fn scrubbed(report: &UiErrorReport, now: u64) -> UiError {
    let optional = |text: Option<&str>, chars, lines| {
        text.map(|text| scrub(text, chars, lines))
            .filter(|text| !text.trim().is_empty())
    };
    UiError {
        at: now,
        kind: report.kind,
        message: scrub(&report.message, MESSAGE_CHARS, MESSAGE_LINES),
        stack: optional(report.stack.as_deref(), STACK_CHARS, STACK_LINES),
        route: report.route.as_deref().and_then(route),
        build: optional(report.build.as_deref(), BUILD_CHARS, 1),
    }
}

/// `text` scrubbed line by line ([`scrub_line`]), cut to `max_lines` lines (a last line `…`
/// says some were left out) and then to `max_chars` characters (ending in `…`). Control
/// characters (an escape, a tab, a carriage return), line and paragraph separators and the
/// marks that turn text direction around become spaces.
pub(crate) fn scrub(text: &str, max_chars: usize, max_lines: usize) -> String {
    let mut lines: Vec<String> = text
        .split('\n')
        .take(max_lines)
        .map(|line| {
            let plain: String = line
                .chars()
                .map(|c| if is_layout_control(c) { ' ' } else { c })
                .collect();
            scrub_line(plain.trim_end())
        })
        .collect();
    if text.split('\n').nth(max_lines).is_some() {
        lines.push(CUT.to_string());
    }
    let out = lines.join("\n");
    if out.chars().count() <= max_chars {
        return out;
    }
    let mut cut: String = out.chars().take(max_chars).collect();
    cut.push(CUT);
    cut
}

fn is_layout_control(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// One line with what could be a secret replaced. Spaces are kept as they were, so a stack
/// frame keeps its indent.
pub(crate) fn scrub_line(line: &str) -> String {
    let words: Vec<&str> = line.split(' ').collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut next = Next::Plain;
    for word in words {
        if word.is_empty() {
            out.push(String::new());
            continue;
        }
        let this = std::mem::replace(&mut next, Next::Plain);
        if this == Next::Value || (this == Next::Credential && looks_like_credential(word)) {
            out.push(replace_core(word, |_| true));
            continue;
        }
        // `password = x` and `password =x`: the name was the word before.
        let after_name = (word.starts_with('=') || word.starts_with(':'))
            && out
                .iter()
                .rev()
                .find(|w| !w.is_empty())
                .is_some_and(|name| is_secret_name(name));
        if after_name && word.len() > 1 {
            out.push(format!("{}{REDACTED}", &word[..1]));
            continue;
        }
        if ends_with_secret_name(word) || after_name {
            next = Next::Value;
        } else if is_auth_scheme(trim_punctuation(word)) {
            next = Next::Credential;
        }
        out.push(if next == Next::Plain {
            scrub_word(word)
        } else {
            word.to_owned()
        });
    }
    out.join(" ")
}

/// What the word after this one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Plain,
    /// The value of a name that says secret (`password: x`): replaced whatever it is.
    Value,
    /// What follows a scheme word (`Bearer x`, `token x`): replaced when it looks like a
    /// credential, so "the token expired" keeps its words.
    Credential,
}

/// Whether `word` could be a credential: 8 or more characters of the kinds tokens use, with a
/// digit or a capital letter in them.
fn looks_like_credential(word: &str) -> bool {
    let core = trim_punctuation(word);
    core.len() >= 8
        && core.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_' | '.' | '~')
        })
        && core
            .chars()
            .any(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// A word with a URL, a `name=value` secret or a token in it made safe.
fn scrub_word(word: &str) -> String {
    if let Some(at) = word.find("://") {
        return scrub_url(word, at);
    }
    if let Some((name, _)) = secret_assignment(word) {
        return format!("{name}{REDACTED}");
    }
    scrub_path(word)
}

/// `path` with each `/`-separated part that looks like a token replaced (a reset link, an
/// invite path).
fn scrub_path(path: &str) -> String {
    path.split('/')
        .map(|part| replace_core(part, looks_like_token))
        .collect::<Vec<_>>()
        .join("/")
}

/// `word` with the URL whose `://` is at `at` reduced to scheme, host and path. A `:line:col`
/// that followed the path (a stack frame) stays, and so does punctuation around the URL.
fn scrub_url(word: &str, at: usize) -> String {
    let start = word[..at]
        .char_indices()
        .rev()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let (lead, url) = word.split_at(start);
    let rest = &url[at - start + 3..];
    let scheme = &url[..at - start];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let (path, dropped) = tail.split_at(path_end);
    let place = trailing_place(dropped);
    let path = scrub_path(path);
    format!("{lead}{scheme}://{host}{path}{place}")
}

/// The `:line:col` and closing punctuation (a bracket, a quote, `”`) that end `text`, if it ends
/// in them.
fn trailing_place(text: &str) -> &str {
    let closing = text.trim_end_matches(|c: char| {
        matches!(c, ')' | ']' | ',' | ';' | '.' | '"' | '\'')
            || (!c.is_ascii() && !c.is_alphanumeric())
    });
    let mut start = closing.len();
    let mut numbers = 0;
    while let Some(colon) = closing[..start].rfind(':') {
        let digits = &closing[colon + 1..start];
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            break;
        }
        start = colon;
        numbers += 1;
    }
    if numbers == 0 {
        &text[closing.len()..]
    } else {
        &text[start..]
    }
}

/// The name part (with its `=` or `:`) and value of `name=value` or `"name":"value"`, when the
/// name says it holds a secret and a value follows.
fn secret_assignment(word: &str) -> Option<(&str, &str)> {
    let split = word.find(['=', ':'])?;
    let (name, value) = (&word[..=split], &word[split + 1..]);
    (is_secret_name(name) && !trim_punctuation(value).is_empty()).then_some((name, value))
}

/// Whether `word` is a name that says secret and ends in `=` or `:`, so the next word is its value.
fn ends_with_secret_name(word: &str) -> bool {
    (word.ends_with(':') || word.ends_with('=')) && is_secret_name(word)
}

fn is_secret_name(name: &str) -> bool {
    /// Words that say secret wherever they stand in a part (`csrftoken`, `sessionid`).
    const INSIDE: [&str; 7] = [
        "token",
        "secret",
        "password",
        "passwd",
        "session",
        "cookie",
        "credential",
    ];
    /// Short words that say secret only as a whole part (`api_key`, not `monkey`).
    const WHOLE: [&str; 4] = ["pwd", "auth", "key", "apikey"];
    let name = trim_punctuation(name.trim_end_matches('='));
    // A name, not a path or a sentence: `api_key`, `"token"`, `X-Auth-Token`.
    let identifier = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    // `Authorization:` is followed by its scheme, which `is_auth_scheme` handles.
    identifier
        && name_parts(name)
            .iter()
            .any(|part| WHOLE.contains(&part.as_str()) || INSIDE.iter().any(|n| part.contains(n)))
}

/// The lower-case parts of an identifier: `X-Auth-Token`, `api_key` and `accessToken` give
/// `x auth token`, `api key` and `access token`.
fn name_parts(name: &str) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut lower_before = false;
    for c in name.chars() {
        if matches!(c, '_' | '-') || (c.is_ascii_uppercase() && lower_before) {
            parts.push(String::new());
        }
        lower_before = c.is_ascii_lowercase() || c.is_ascii_digit();
        if c.is_ascii_alphanumeric()
            && let Some(last) = parts.last_mut()
        {
            last.push(c.to_ascii_lowercase());
        }
    }
    parts.retain(|part| !part.is_empty());
    parts
}

fn is_auth_scheme(word: &str) -> bool {
    ["bearer", "basic", "token", "negotiate", "ntlm", "digest"]
        .iter()
        .any(|s| word.eq_ignore_ascii_case(s))
}

/// Whether `core` (a word without its punctuation) is shaped like a token: a known prefix, a
/// JWT, or 32 or more letters, digits, `-`, `_`, `+` or `=` with both letters and digits.
fn looks_like_token(core: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "glpat-",
        "xoxb-",
    ];
    let token_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '=');
    if PREFIXES.iter().any(|p| core.starts_with(p)) && core.len() > 12 {
        return true;
    }
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() == 3
        && core.starts_with("eyJ")
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(token_char))
    {
        return true;
    }
    core.len() >= 32
        && core.chars().all(token_char)
        && core.chars().any(|c| c.is_ascii_digit())
        && core.chars().any(|c| c.is_ascii_alphabetic())
}

/// `word` with its core (without leading and trailing punctuation) replaced when `test` says so.
fn replace_core(word: &str, test: impl Fn(&str) -> bool) -> String {
    let core = trim_punctuation(word);
    if core.is_empty() || !test(core) {
        return word.to_owned();
    }
    let start = word.len() - word.trim_start_matches(is_punctuation).len();
    format!(
        "{}{REDACTED}{}",
        &word[..start],
        &word[start + core.len()..]
    )
}

fn trim_punctuation(word: &str) -> &str {
    word.trim_matches(is_punctuation)
}

fn is_punctuation(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ',' | ';' | '.' | ':' | '<' | '>' | '`'
    )
}

/// The path of a page address: no query or fragment, one line, bounded; `None` when empty.
pub(crate) fn route(text: &str) -> Option<String> {
    let path = text.split(['?', '#']).next().unwrap_or("");
    let path = scrub(&path.replace('\n', " "), ROUTE_CHARS, 1);
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests;
