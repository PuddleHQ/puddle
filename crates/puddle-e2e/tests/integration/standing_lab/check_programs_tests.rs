// SPDX-License-Identifier: AGPL-3.0-or-later
use super::check_programs;

fn with<'a>(vars: &'a [&'a str]) -> impl Fn(&str) -> bool + 'a {
    move |name| vars.contains(&name)
}

#[test]
fn a_required_mitmproxy_alone_fails_when_it_is_missing() {
    let err = check_programs(&with(&["PUDDLE_REQUIRE_MITMPROXY", "PUDDLE_SQUID_BIN"])).unwrap_err();
    assert!(err.contains("PUDDLE_MITMDUMP_BIN"), "{err}");
}

#[test]
fn a_required_squid_alone_fails_when_it_is_missing() {
    let err = check_programs(&with(&["PUDDLE_REQUIRE_SQUID", "PUDDLE_MITMDUMP_BIN"])).unwrap_err();
    assert!(err.contains("PUDDLE_SQUID_BIN"), "{err}");
}

#[test]
fn nothing_required_and_one_missing_skips() {
    assert_eq!(check_programs(&with(&["PUDDLE_MITMDUMP_BIN"])), Ok(false));
    assert_eq!(check_programs(&with(&[])), Ok(false));
}

#[test]
fn both_configured_runs() {
    let vars = [
        "PUDDLE_SQUID_DOCKER",
        "PUDDLE_MITMDUMP_BIN",
        "PUDDLE_REQUIRE_SQUID",
    ];
    assert_eq!(check_programs(&with(&vars)), Ok(true));
}
