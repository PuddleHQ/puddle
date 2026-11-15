// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules on the method and path of a request (`docs/spec/rules.md` R-49..R-55).
//!
//! A request rule names a host pattern, the methods it covers (none: every method) and a path
//! pattern, and it only ever refuses. It is checked on every request the proxy can read: the
//! requests of a decrypted connection (each request of a keep-alive or HTTP/2 connection on its
//! own) and plain-HTTP requests. A host with a request rule is always decrypted, because a
//! request that cannot be read cannot be judged: [`RequestRules::hosts`] is added to the hosts a
//! workspace decrypts.

use puddle_types::{CanonicalPath, Host, Methods, PathPattern, RuleId, percent_decoded};

use super::{PatternError, RequestView, TerminationSet};
use crate::proxy::Refusal;

/// Headers by which a client asks a server to read a request as another method. Honoured
/// by common frameworks, so a rule on `DELETE` has to see through `POST` plus one of these.
const METHOD_OVERRIDES: [&str; 3] = [
    "x-http-method-override",
    "x-http-method",
    "x-method-override",
];

/// One rule on method and path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRule {
    id: RuleId,
    hosts: TerminationSet,
    methods: Methods,
    path: PathPattern,
}

impl RequestRule {
    /// A rule for requests to `host` (`example.com` or `*.example.com`).
    ///
    /// # Errors
    /// [`PatternError`] when `host` is not a name or is too wide.
    pub fn new(
        id: RuleId,
        host: &str,
        methods: Methods,
        path: PathPattern,
    ) -> Result<Self, PatternError> {
        Ok(Self {
            id,
            hosts: TerminationSet::parse([host])?,
            methods,
            path,
        })
    }

    fn refuses(&self, host: &Host, methods: &[&str], paths: &[CanonicalPath]) -> bool {
        self.hosts.contains(host)
            && self.methods.matches_any(methods.iter().copied())
            && paths.iter().any(|path| self.path.matches(path))
    }
}

/// The request rules that apply to one workspace: the global ones and its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestRules {
    rules: Vec<RequestRule>,
}

impl RequestRules {
    /// The rules, in the order they are asked (lowest id first).
    #[must_use]
    pub fn new(mut rules: Vec<RequestRule>) -> Self {
        rules.sort_by_key(|rule| rule.id);
        Self { rules }
    }

    /// Whether there is no rule.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The hosts the rules name: the hosts a workspace must decrypt for them to work.
    #[must_use]
    pub fn hosts(&self) -> TerminationSet {
        let mut all = TerminationSet::new();
        for rule in &self.rules {
            all.extend(&rule.hosts);
        }
        all
    }

    /// Whether any rule names `host`.
    #[must_use]
    pub fn covers(&self, host: &Host) -> bool {
        self.covering(host).is_some()
    }

    /// The lowest-numbered rule that names `host`, or `None` when none does.
    #[must_use]
    pub fn covering(&self, host: &Host) -> Option<RuleId> {
        self.rules
            .iter()
            .find(|rule| rule.hosts.contains(host))
            .map(|rule| rule.id)
    }

    /// The rule that refuses `request` to `host`, the lowest id first, or `None` when none does.
    ///
    /// The path is compared in every canonical form a server may read it in (see
    /// [`CanonicalPath::readings`]). The method is the one sent and also every method the
    /// request asks to be read as: an override header (`X-HTTP-Method-Override` and two like it)
    /// or a `_method` in the query string, read as PHP reads it (pairs split at `&` or `;`, a `+`
    /// is a space, `.` and a space in a key are `_`) and also with a `+` in the value kept as
    /// written. A rule on `DELETE` therefore refuses a `POST` that asks to be a `DELETE`.
    #[must_use]
    pub fn refusing(&self, host: &Host, request: &RequestView<'_>) -> Option<RuleId> {
        if !self.covers(host) {
            return None;
        }
        let paths = CanonicalPath::readings(request.target());
        let mut methods: Vec<&str> = vec![request.method()];
        for name in METHOD_OVERRIDES {
            for value in request.header_values(name) {
                methods.extend(value.split(',').map(str::trim));
            }
        }
        let form = |text: &str| percent_decoded(&text.replace('+', " "));
        let asked: Vec<String> = request
            .query()
            .into_iter()
            .flat_map(|query| query.split(['&', ';']))
            .filter_map(|pair| pair.split_once('='))
            .filter(|(key, _)| {
                form(key)
                    .trim_start()
                    .replace(['.', ' '], "_")
                    .eq_ignore_ascii_case("_method")
            })
            .flat_map(|(_, value)| {
                // `+` is a space to PHP but a token character in a method name.
                [form(value), percent_decoded(value)].map(|method| method.trim().to_owned())
            })
            .collect();
        methods.extend(asked.iter().map(String::as_str));
        self.rules
            .iter()
            .find(|rule| rule.refuses(host, &methods, &paths))
            .map(|rule| rule.id)
    }
}

/// The request a rule refused, for the connection's audit record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refused {
    pub(crate) rule: RuleId,
    pub(crate) line: puddle_types::HttpRequestLine,
}

/// The answer for a request the rule `id` refused: `403`, with the decision and the rule in
/// headers a tool can read. The path is not repeated, so nothing of it lands in a log line the
/// guest's tool prints.
pub(crate) fn refusal(id: RuleId, method: &str, host: &Host) -> Refusal {
    let method: String = method
        .chars()
        .filter(char::is_ascii_graphic)
        .take(32)
        .collect();
    Refusal::new(
        "403 Forbidden",
        format!("a rule refuses {method} requests like this one to {host} (request rule {id})"),
    )
    .header("x-puddle-decision", "deny")
    .header("x-puddle-request-rule", id.to_string())
}

#[cfg(test)]
mod tests;
