// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules on method and path and what a learned step reports.

use super::*;


#[tokio::test]
async fn the_request_rules_scenario_seeds_rules_on_method_and_path_and_a_request_one_refused() {
    let run = start("request-rules").await;

    let all = run.get("/api/request-rules").await.json();
    let rules = all["rules"].as_array().unwrap();
    assert_eq!(rules.len(), 2, "{all}");
    assert_eq!(rules[0]["host"], "api.example.com");
    assert_eq!(rules[0]["methods"], json!(["DELETE"]));
    assert_eq!(rules[0]["path"], "/repos/**");
    assert_eq!(rules[0]["workspace"], Value::Null);
    assert_eq!(rules[1]["workspace"], "docs-site");
    assert_eq!(rules[1]["methods"], json!(["POST", "PUT"]));

    let own = run
        .get("/api/request-rules?workspace=web-shop")
        .await
        .json();
    assert_eq!(own["rules"].as_array().unwrap().len(), 1, "{own}");

    let audit = run.get("/api/audit").await.json();
    let refused = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["record"])
        .find(|r| r["reason"] == "request_rule")
        .unwrap_or_else(|| panic!("no refused request in {audit}"));
    assert_eq!(refused["decision"], "deny");
    assert_eq!(refused["method"], "DELETE");
    assert_eq!(refused["path"], "/repos/acme/web-shop");
    assert_eq!(refused["request_rule"], 1);
}

#[tokio::test]
async fn a_scenario_with_a_bad_rule_on_method_and_path_fails_to_start_and_names_it() {
    for (seed, word) in [
        (
            json!({"host": "a.example", "methods": ["GE T"], "path": "/"}),
            "GE T",
        ),
        (json!({"host": "a.example", "path": "/a*"}), "whole segment"),
        (json!({"host": "", "path": "/"}), "names a host"),
        (
            json!({"host": "a.example", "path": "/", "workspace": "Bad Name"}),
            "Bad Name",
        ),
    ] {
        let scenario: Scenario = serde_json::from_value(json!({"request_rules": [seed]})).unwrap();
        let err = Fixture::start(FixtureOptions {
            port: 0,
            connection_file: None,
            scenario,
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains(word), "{word}: {err}");
    }
}

#[tokio::test]
async fn a_learned_step_carries_how_often_the_host_was_refused() {
    let run = start("environment").await;
    let body = json!({"do": "learned", "workspace": "web-shop", "host": "again.example", "expires_in_secs": 120, "count": 3});
    let mut events = run.events().await;
    let step = run.control("POST", "/control/step", Some(&body)).await;
    assert_eq!(step.status, 204, "{}", step.body);
    let seen = read_until(&mut events, |text| text.contains("again.example")).await;
    assert!(seen.contains(r#""count":3"#), "{seen}");
    let learned = run
        .get("/api/workspaces/web-shop/interception/learned")
        .await
        .json();
    let again = &learned["hosts"][0];
    assert_eq!(again["host"], "again.example", "{learned}");
    assert_eq!(again["count"], 3, "{learned}");
    assert!(
        again["first_rejected_at"].as_u64().unwrap() < again["last_rejected_at"].as_u64().unwrap(),
        "{learned}"
    );
}
