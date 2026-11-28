// SPDX-License-Identifier: AGPL-3.0-or-later
//! Connection history and the network health report.

use super::*;

#[tokio::test]
async fn a_history_step_writes_connection_records_across_a_week() {
    let run = start("empty").await;
    let step = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "history", "count": 600})),
        )
        .await;
    assert_eq!(step.status, 204, "{}", step.body);
    let now = run.state().await["now_ms"].as_u64().unwrap();
    // Connections only: the fixture's API also records System managed when it starts.
    let page = run.get("/api/audit?type=connection&limit=500").await.json();
    let entries = page["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 500);
    // Newest first, and the newest is just before now; ids and times fall together.
    let ts = |i: usize| entries[i]["record"]["ts"].as_u64().unwrap();
    assert!(ts(0) <= now && now - ts(0) < 7 * 24 * 3_600_000 / 600 + 1);
    assert!(ts(0) > ts(499));
    let oldest = run
        .get(&format!(
            "/api/audit?type=connection&before={}&limit=500",
            page["next_before"]
        ))
        .await
        .json();
    assert_eq!(oldest["entries"].as_array().unwrap().len(), 100);
    assert_eq!(oldest["next_before"], Value::Null);
    let mut decisions: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["record"]["decision"].as_str())
        .collect();
    decisions.sort_unstable();
    decisions.dedup();
    assert_eq!(decisions, ["allow", "blocked", "deny", "pending"]);
}

#[tokio::test]
async fn the_network_health_report_is_seeded_stamped_and_replaced_by_a_step() {
    let run = start("corporate-network").await;
    let report = run.get("/api/network-health").await;
    assert_eq!(report.status, 200, "{}", report.body);
    let body = report.json();
    assert_eq!(body["generated_at"], run.fixture.now_ms().await);
    assert_eq!(body["proxy"]["detected"], "pac");
    assert_eq!(
        body["proxy"]["pac_url"],
        "http://wpad.corp.example/proxy.pac"
    );
    assert_eq!(body["sign_in"]["attempts"][0]["result"], "signed_in");
    assert_eq!(body["roots"]["roots"], 2);
    assert_eq!(body["pull_proxy"]["via_upstream"], true);
    assert_eq!(body["routes"].as_array().unwrap().len(), 3);

    let mut stream = run.events().await;
    let ran = run
        .control("POST", "/control/script/network-change", None)
        .await;
    assert_eq!(ran.status, 204, "{}", ran.body);
    let seen = read_until(&mut stream, |s| s.contains("network_changed")).await;
    assert!(seen.contains("\"epoch\":4"), "{seen}");
    let body = run.get("/api/network-health").await.json();
    assert_eq!(body["proxy"]["epoch"], 4);
    assert_eq!(body["proxy"]["dead_proxies"], json!([]));
    assert_eq!(body["routes"], json!([]));

    // A reset starts over from the scenario; the empty one is a machine with no proxy.
    run.fixture
        .reset(Some(load_scenario("empty").unwrap()))
        .await
        .unwrap();
    let body = run.get("/api/network-health").await.json();
    assert_eq!(body["proxy"]["detected"], "direct");
    assert_eq!(body["roots"]["synced"], false);
}

#[tokio::test]
async fn the_trouble_scenario_shows_what_a_broken_setup_reports() {
    let run = start("network-trouble").await;
    let body = run.get("/api/network-health").await.json();
    assert_eq!(body["proxy"]["pac_state"], "unreachable");
    assert_eq!(body["sign_in"]["attempts"][0]["result"], "failed");
    assert_eq!(body["roots"]["synced"], false);
    assert!(
        body["roots"]["unreadable_stores"][0]
            .as_str()
            .unwrap()
            .contains("access denied")
    );
}

#[tokio::test]
async fn a_network_health_report_with_a_typo_fails_to_start() {
    let mut scenario = load_scenario("corporate-network").unwrap();
    let mut report = serde_json::to_value(scenario.network_health.take().unwrap()).unwrap();
    report["proxy"]["pac_ur"] = json!("http://x/");
    let text = json!({"name": "typo", "network_health": report}).to_string();
    let err = serde_json::from_str::<Scenario>(&text)
        .unwrap_err()
        .to_string();
    assert!(err.contains("pac_ur"), "{err}");
}
