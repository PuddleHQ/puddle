// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::{Error, ErrorKind};

use super::*;

#[test]
fn a_direct_connect_that_timed_out_is_a_timeout_and_any_other_failure_is_a_connect_failure() {
    assert_eq!(
        connect_outcome(&Error::new(ErrorKind::TimedOut, "connect timed out")),
        ConnectionOutcome::Timeout
    );
    for kind in [ErrorKind::ConnectionRefused, ErrorKind::NotFound] {
        assert_eq!(
            connect_outcome(&Error::from(kind)),
            ConnectionOutcome::ConnectFailed
        );
    }
}

fn blocked_by(refusal: &Refusal) -> Option<&str> {
    refusal
        .headers
        .iter()
        .find(|(name, _)| *name == "x-puddle-blocked-by")
        .map(|(_, value)| value.as_str())
}

#[test]
fn a_sign_in_that_failed_and_one_that_is_missing_are_refused_apart_without_naming_the_proxy() {
    let target = Target {
        host: puddle_netpolicy::normalise_host("example.com")
            .unwrap()
            .into_host(),
        port: 443,
    };
    let proxy = puddle_upstream::ProxyAddr::new("corp-proxy.internal", 3128);

    let failed = refusal(
        &ChainError::AuthFailed {
            proxy: proxy.clone(),
            why: "rejected".to_owned(),
        },
        &target,
    );
    assert_eq!(failed.status, "502 Bad Gateway");
    assert_eq!(blocked_by(&failed), Some("upstream-auth"));
    assert!(failed.message.contains("signing in"), "{}", failed.message);

    let required = refusal(
        &ChainError::AuthRequired {
            proxy,
            schemes: vec!["Negotiate".to_owned()],
        },
        &target,
    );
    assert_eq!(blocked_by(&required), Some("upstream-auth"));
    assert!(
        required.message.contains("Negotiate"),
        "{}",
        required.message
    );
    assert_ne!(failed.message, required.message);

    for refusal in [&failed, &required] {
        assert!(
            !refusal.message.contains("corp-proxy"),
            "{}",
            refusal.message
        );
    }
}

#[test]
fn a_company_proxy_that_does_not_answer_is_named_as_such_and_a_route_with_none_keeps_the_old_words()
{
    let target = Target {
        host: puddle_netpolicy::normalise_host("example.com")
            .unwrap()
            .into_host(),
        port: 443,
    };
    let proxy = puddle_upstream::ProxyAddr::new("corp-proxy.internal", 3128);
    let tried = |hop: Hop| ChainError::Unreachable {
        tried: vec![(hop, "connection refused".to_owned())],
        timed_out: false,
    };

    let through_proxy = refusal(&tried(Hop::Proxy(proxy)), &target);
    assert_eq!(through_proxy.status, "502 Bad Gateway");
    assert!(
        through_proxy
            .message
            .contains("company proxy did not answer"),
        "{}",
        through_proxy.message
    );
    assert!(
        !through_proxy.message.contains("corp-proxy"),
        "the address stays in the log: {}",
        through_proxy.message
    );

    let direct = refusal(&tried(Hop::Direct), &target);
    assert!(
        direct.message.contains("no route to it works"),
        "{}",
        direct.message
    );
}
