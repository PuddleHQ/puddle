// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn gh_json_keeps_names_and_state_only() {
    let json = r#"{"hosts":{"github.com":[
            {"login":"me","active":true,"state":"success","token":"CANARY-x","scopes":"repo"},
            {"login":"old","active":false,"state":"error"},
            {"login":"-bad"}],
          "bad host":[{"login":"x"}]}}"#;
    let mut got = parse_gh(json);
    got.sort();
    assert_eq!(got.len(), 2);
    assert!(
        got.iter()
            .any(|a| a.account.as_str() == "me" && a.signed_in)
    );
    assert!(
        got.iter()
            .any(|a| a.account.as_str() == "old" && !a.signed_in)
    );
    assert!(!format!("{got:?}").contains("CANARY"));
    assert_eq!(parse_gh("not json").len(), 0);
}

#[test]
fn gcm_github_lines() {
    let got = parse_gcm_github("me\n  other  \npassword CANARY-y\n\n");
    let names: Vec<_> = got.iter().map(|a| a.account.as_str()).collect();
    assert_eq!(names, ["me", "other"]);
}

#[test]
fn gcm_azure_bindings() {
    let text = "acme:\n  (global) -> me@example.com\n  (local)  -> you@example.com\nother:\n  \
                    (global) -> x\n  (system) -> sys\n  token=CANARY-z\nstray: junk\n  (global) -> orphan\n";
    let got = parse_gcm_azure(text);
    let pairs: Vec<_> = got
        .iter()
        .map(|a| (a.org.as_ref().unwrap().as_str(), a.account.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("acme", "me@example.com"),
            ("acme", "you@example.com"),
            ("other", "x")
        ]
    );
}
