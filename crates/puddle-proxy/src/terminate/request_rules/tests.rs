// SPDX-License-Identifier: AGPL-3.0-or-later
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
