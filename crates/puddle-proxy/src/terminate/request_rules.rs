// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use super::*;

    fn host(text: &str) -> Host {
        Host::parse_normalised(text).unwrap()
    }

    fn rule(id: i64, host: &str, methods: &[&str], path: &str) -> RequestRule {
        RequestRule::new(
            RuleId(id),
            host,
            Methods::parse(methods.iter().copied()).unwrap(),
            PathPattern::parse(path).unwrap(),
        )
        .unwrap()
    }

    fn refusing(rules: &RequestRules, host_name: &str, method: &str, target: &str) -> Option<i64> {
        let view = RequestView::new(method, target, &[]);
        rules.refusing(&host(host_name), &view).map(|id| id.0)
    }

    #[test]
    fn a_rule_covers_its_host_and_a_wildcard_its_subdomains_only() {
        let rules = RequestRules::new(vec![
            rule(1, "api.example.com", &["DELETE"], "/**"),
            rule(2, "*.example.org", &[], "/admin"),
        ]);
        assert_eq!(refusing(&rules, "api.example.com", "DELETE", "/x"), Some(1));
        assert_eq!(refusing(&rules, "api.example.com", "GET", "/x"), None);
        assert_eq!(refusing(&rules, "other.example.com", "DELETE", "/x"), None);
        assert_eq!(refusing(&rules, "a.example.org", "GET", "/ADMIN"), Some(2));
        assert_eq!(
            refusing(&rules, "a.b.example.org", "GET", "/admin"),
            Some(2)
        );
        assert_eq!(refusing(&rules, "example.org", "GET", "/admin"), None);
        assert_eq!(refusing(&rules, "10.0.0.1", "GET", "/admin"), None);
        assert!(rules.covers(&host("a.example.org")));
        assert!(!rules.covers(&host("example.org")));
        assert_eq!(rules.covering(&host("a.example.org")), Some(RuleId(2)));
        assert_eq!(rules.covering(&host("example.org")), None);
        let hosts = rules.hosts();
        assert!(hosts.contains(&host("api.example.com")) && hosts.contains(&host("x.example.org")));
        assert!(!hosts.contains(&host("example.org")));
    }

    #[test]
    fn rules_that_cannot_be_made_are_refused() {
        let any = || (Methods::any(), PathPattern::any());
        for bad in ["*.com", "10.0.0.1", "", "a b", "*"] {
            let (m, p) = any();
            assert!(RequestRule::new(RuleId(1), bad, m, p).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn no_rules_refuse_nothing() {
        let rules = RequestRules::default();
        assert!(rules.is_empty());
        assert_eq!(refusing(&rules, "a.example.com", "DELETE", "/"), None);
        assert!(rules.hosts().is_empty());
        assert!(!RequestRules::new(vec![rule(1, "a.example.com", &[], "/admin")]).is_empty());
    }

    #[test]
    fn the_target_may_be_in_absolute_form_and_carry_a_query_and_the_path_is_not_its_scheme() {
        let rules = RequestRules::new(vec![rule(1, "a.example.com", &[], "/admin")]);
        for target in [
            "http://a.example.com/admin?x",
            "https://a.example.com/%41DMIN#f",
            "/admin?next=http://elsewhere/x",
        ] {
            assert_eq!(
                refusing(&rules, "a.example.com", "GET", target),
                Some(1),
                "{target}"
            );
        }
        // `://` inside an origin-form path belongs to the path
        assert_eq!(
            refusing(&rules, "a.example.com", "GET", "/x://y/admin"),
            None
        );
    }

    #[test]
    fn a_method_named_by_a_header_or_the_query_is_also_judged() {
        let rules = RequestRules::new(vec![rule(1, "a.example.com", &["DELETE"], "/**")]);
        let host = host("a.example.com");
        let headers = |line: &str| vec![line.to_owned()];
        for lines in [
            headers("X-HTTP-Method-Override: DELETE"),
            headers("x-http-method: delete"),
            headers("X-Method-Override: put,delete"),
            headers("X-Method-Override:  DELETE "),
        ] {
            let view = RequestView::new("POST", "/x", &lines);
            assert_eq!(rules.refusing(&host, &view), Some(RuleId(1)), "{lines:?}");
        }
        for target in [
            "/x?_method=DELETE",
            "/x?a=b&_METHOD=%64elete",
            "/x?_method=%2544ELETE",
        ] {
            let view = RequestView::new("POST", target, &[]);
            assert_eq!(rules.refusing(&host, &view), Some(RuleId(1)), "{target}");
        }
        for target in [
            "/x?method=DELETE",
            "/x?_method",
            "/x?_method=GET",
            "/x?a=_method=DELETE",
        ] {
            let view = RequestView::new("POST", target, &[]);
            assert_eq!(rules.refusing(&host, &view), None, "{target}");
        }
        let other = headers("x-http-method-override: PATCH");
        let view = RequestView::new("POST", "/x", &other);
        assert_eq!(rules.refusing(&host, &view), None);
    }

    #[test]
    fn a_plus_in_a_method_named_by_the_query_is_read_as_written_and_as_a_space() {
        let rules = RequestRules::new(vec![rule(1, "a.example.com", &["FOO+BAR"], "/**")]);
        let host = host("a.example.com");
        for target in ["/x?_method=FOO+BAR", "/x?_method=foo%2Bbar"] {
            let view = RequestView::new("POST", target, &[]);
            assert_eq!(rules.refusing(&host, &view), Some(RuleId(1)), "{target}");
        }
        let view = RequestView::new("POST", "/x?_method=FOO+BAZ", &[]);
        assert_eq!(rules.refusing(&host, &view), None);
    }

    #[test]
    fn a_hash_is_judged_both_as_a_fragment_and_as_part_of_the_path() {
        let rules = RequestRules::new(vec![rule(1, "a.example.com", &[], "/admin/**")]);
        for target in [
            "/admin#top",
            "/admin/x#top",
            "/public#/../admin",
            "/public%23/../admin?q=1",
        ] {
            assert_eq!(
                refusing(&rules, "a.example.com", "GET", target),
                Some(1),
                "{target}"
            );
        }
        assert_eq!(
            refusing(&rules, "a.example.com", "GET", "/public#/admin"),
            None
        );
        assert_eq!(
            refusing(&rules, "a.example.com", "GET", "/public?x=#/../admin"),
            None,
            "the query ends the path before the `#`"
        );
    }

    #[test]
    fn the_answer_names_the_rule_and_the_method_but_never_the_path() {
        let refusal = refusal(RuleId(4), "DELETE", &host("a.example.com"));
        assert_eq!(refusal.status, "403 Forbidden");
        assert!(refusal.message.contains("DELETE") && refusal.message.contains("rule 4"));
        let sneaky = refusal_for_method("DE\r\nX: y");
        assert!(!sneaky.message.contains('\r') && !sneaky.message.contains('\n'));
    }

    fn refusal_for_method(method: &str) -> Refusal {
        refusal(RuleId(1), method, &host("a.example.com"))
    }
}
