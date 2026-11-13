// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::hop::{Destination, Scheme};

fn env(pairs: &[(&str, &str)]) -> EnvOs {
    EnvOs::from_vars(
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    )
}

fn rules(e: &EnvOs, scheme: Scheme) -> Option<ProxyAddr> {
    e.rules.for_scheme(scheme).cloned()
}

#[test]
fn variables_pick_the_scheme_then_all_and_lower_case_wins() {
    let e = env(&[
        ("HTTPS_PROXY", "http://a:1"),
        ("https_proxy", "http://b:2"),
        ("ALL_PROXY", "c:3"),
        ("HTTP_PROXY", "d"),
    ]);
    assert_eq!(rules(&e, Scheme::Https), Some(ProxyAddr::new("b", 2)));
    assert_eq!(rules(&e, Scheme::Http), Some(ProxyAddr::new("d", 80)));
    assert_eq!(
        rules(&env(&[("all_proxy", "c:3")]), Scheme::Http),
        Some(ProxyAddr::new("c", 3))
    );
    assert_eq!(rules(&env(&[]), Scheme::Https), None);
}

#[test]
fn unusable_values_are_ignored_and_credentials_dropped() {
    let e = env(&[
        ("HTTPS_PROXY", "socks5://p:1080"),
        ("HTTP_PROXY", "  "),
        ("ALL_PROXY", "http://u:pw@p.corp:8080"),
    ]);
    assert_eq!(
        rules(&e, Scheme::Https),
        Some(ProxyAddr::new("p.corp", 8080))
    );
    assert!(!format!("{e:?}").contains("pw"));
}

#[test]
fn an_unusable_variable_is_listed_as_a_problem_with_its_name_and_no_credential() {
    let e = env(&[
        ("HTTPS_PROXY", "socks5://user:hunter2@p:1080"),
        ("HTTP_PROXY", "http://good.corp:3128"),
        ("ALL_PROXY", "p:notaport"),
    ]);
    let config = e.config();
    assert_eq!(config.problems.len(), 2, "{:?}", config.problems);
    let shown = format!("{:?}", config.problems);
    assert!(
        shown.contains("HTTPS_PROXY") && shown.contains("socks5"),
        "{shown}"
    );
    assert!(
        shown.contains("ALL_PROXY") && shown.contains("p:notaport"),
        "{shown}"
    );
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(
        config
            .problems
            .iter()
            .all(|p| p.kind == crate::health::ProxyProblemKind::UnusableSetting)
    );
    assert_eq!(env(&[("HTTPS_PROXY", "p:1")]).config().problems, vec![]);
}

#[test]
fn a_system_layer_that_cannot_be_read_is_replaced_by_the_environment_and_says_so() {
    let os = crate::FakeOs::new(ProxyConfig::default());
    os.fail_config("registry access denied");
    let fallback = EnvFallback::new(os, env(&[("HTTPS_PROXY", "e.corp:3128")]));
    let config = fallback.config().unwrap();
    assert_eq!(config.origin, Origin::Environment);
    assert_eq!(
        config.rules.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("e.corp", 3128))
    );
    assert_eq!(config.read_error.as_deref(), Some("registry access denied"));
}

#[test]
fn environment_problems_count_only_when_the_environment_is_used() {
    let unusable = [("HTTPS_PROXY", "socks5://p:1")];
    let os = crate::FakeOs::new(ProxyConfig::default());
    let used = EnvFallback::new(os, env(&unusable)).config().unwrap();
    assert_eq!(used.problems.len(), 1);
    let system = ProxyConfig {
        rules: ProxyRules::new(Some(ProxyAddr::new("sys", 1)), None, None),
        ..ProxyConfig::default()
    };
    let os = crate::FakeOs::new(system);
    let unused = EnvFallback::new(os, env(&unusable)).config().unwrap();
    assert_eq!(unused.problems, vec![]);
    assert!(unused.read_error.is_none());
}

#[test]
fn no_proxy_becomes_the_bypass_list() {
    let e = env(&[("NO_PROXY", "corp.test"), ("HTTPS_PROXY", "p:1")]);
    let config = e.config();
    assert_eq!(config.origin, Origin::Environment);
    assert!(
        config
            .bypass
            .matches(&Destination::new(Scheme::Https, "a.corp.test", 443))
    );
    assert!(
        !config
            .bypass
            .matches(&Destination::new(Scheme::Https, "github.com", 443))
    );
}

#[test]
fn the_unix_os_layer_is_the_environment_with_no_pac_and_no_watch() {
    let e = env(&[("HTTPS_PROXY", "p:1")]);
    let config = OsProxy::config(&e).unwrap();
    assert!(config.pac_url.is_none() && !config.auto_detect);
    assert_eq!(
        config.rules.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("p", 1))
    );
    let query = PacQuery {
        pac_url: None,
        auto_detect: true,
        url: "http://x/".into(),
        timeout: std::time::Duration::from_secs(1),
    };
    assert!(matches!(
        e.resolve_pac(&query),
        Err(PacError::Unavailable(_))
    ));
    assert!(e.watch(Arc::new(|| {}), Arc::new(|_| {})).is_none());
    let _ = EnvOs::from_process();
}
