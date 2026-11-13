// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::hop::{Destination, ProxyAddr, Scheme};

#[test]
fn the_users_proxy_wins_over_the_machines_and_keeps_its_own_bypass() {
    let config = to_config(WinSettings {
        proxy_server: Some("http=a:1;https=b:2".into()),
        proxy_override: Some("*.corp.test;<local>".into()),
        machine_proxy: Some("m:9".into()),
        machine_bypass: Some("*.m.test".into()),
        auto_config_url: Some("http://pac/p.pac".into()),
        auto_detect: true,
        per_machine_policy: true,
        ..WinSettings::default()
    });
    assert_eq!(config.pac_url.as_deref(), Some("http://pac/p.pac"));
    assert!(config.auto_detect);
    assert_eq!(
        config.rules.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("b", 2))
    );
    assert!(
        config
            .bypass
            .matches(&Destination::new(Scheme::Https, "wiki.corp.test", 443))
    );
    assert!(
        !config
            .bypass
            .matches(&Destination::new(Scheme::Https, "a.m.test", 443))
    );
}

#[test]
fn the_machine_proxy_is_used_when_the_user_has_none() {
    let config = to_config(WinSettings {
        machine_proxy: Some("m:9".into()),
        machine_bypass: Some("*.m.test".into()),
        ..WinSettings::default()
    });
    assert_eq!(
        config.rules.for_scheme(Scheme::Http),
        Some(&ProxyAddr::new("m", 9))
    );
    assert!(
        config
            .bypass
            .matches(&Destination::new(Scheme::Https, "a.m.test", 443))
    );
    assert!(to_config(WinSettings::default()).rules.is_empty());
}

#[test]
fn a_machine_proxy_that_could_not_be_read_is_reported_not_just_ignored() {
    let config = to_config(WinSettings {
        machine_error: Some("access denied".into()),
        ..WinSettings::default()
    });
    assert!(config.rules.is_empty());
    let why = config.read_error.expect("the read error is carried");
    assert!(
        why.contains("machine-wide") && why.contains("access denied"),
        "{why}"
    );
    assert!(to_config(WinSettings::default()).read_error.is_none());
}

#[test]
fn a_proxy_server_entry_puddle_cannot_use_is_reported_with_the_entry() {
    let config = to_config(WinSettings {
        proxy_server: Some("http=good:8080;https=bad:port".into()),
        ..WinSettings::default()
    });
    assert_eq!(
        config.rules.for_scheme(Scheme::Http),
        Some(&ProxyAddr::new("good", 8080))
    );
    assert_eq!(config.problems.len(), 1, "{:?}", config.problems);
    assert!(config.problems[0].detail.contains("https=bad:port"));
    let socks = to_config(WinSettings {
        proxy_server: Some("socks=s:1080".into()),
        ..WinSettings::default()
    });
    assert_eq!(socks.problems.len(), 1, "{:?}", socks.problems);
}
