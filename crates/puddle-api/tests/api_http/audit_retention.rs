// SPDX-License-Identifier: AGPL-3.0-or-later
//! The activity log's retention in days and its size, as the owner reads and sets them.
use serde_json::json;

use crate::common::start;

#[tokio::test]
async fn the_log_is_kept_ninety_days_until_set_and_the_size_is_reported() {
    let api = start().await;
    let size = api.get("/api/audit/retention").await;
    assert_eq!(size.status, 200, "{}", size.body);
    let size = size.json();
    assert_eq!(size["retention_days"], 90);
    assert!(size["records"].as_u64().is_some());
    assert!(size["warn_bytes"].as_u64().unwrap() > 0);
    assert!(size["file_bytes"].as_u64().unwrap() > 0);

    let put = api
        .send("PUT", "/api/audit/retention", Some(&json!({"days": 30})))
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.json()["retention_days"], 30);
    assert_eq!(
        api.get("/api/audit/retention").await.json()["retention_days"],
        30
    );
}

#[tokio::test]
async fn days_outside_one_to_3650_are_refused_and_change_nothing() {
    let api = start().await;
    for bad in [
        json!({"days": 0}),
        json!({"days": 3651}),
        json!({"days": -1}),
    ] {
        let refused = api.send("PUT", "/api/audit/retention", Some(&bad)).await;
        assert!(
            matches!(refused.status, 422 | 400),
            "{bad}: {} {}",
            refused.status,
            refused.body
        );
    }
    assert_eq!(
        api.get("/api/audit/retention").await.json()["retention_days"],
        90
    );
}

/// The `audit_retention_changed` records of the activity log, oldest first.
async fn retention_changes(api: &crate::common::Api) -> Vec<serde_json::Value> {
    let page = api.get("/api/audit").await;
    assert_eq!(page.status, 200, "{}", page.body);
    let mut found: Vec<_> = page.json()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["record"].clone())
        .filter(|record| record["op"] == "audit_retention_changed")
        .collect();
    found.reverse();
    found
}

#[tokio::test]
async fn a_changed_retention_is_recorded_with_both_values_and_who_asked() {
    let api = start().await;
    for days in [30, 30, 7] {
        let put = api
            .send("PUT", "/api/audit/retention", Some(&json!({ "days": days })))
            .await;
        assert_eq!(put.status, 200, "{}", put.body);
    }
    let found = retention_changes(&api).await;
    // The second 30 changed nothing, so it writes nothing.
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0]["type"], "workspace_op");
    assert_eq!(found[0]["actor"], "api");
    assert_eq!(found[0]["outcome"], "succeeded");
    assert_eq!(
        found[0]["detail"],
        "the activity log is kept for 30 days, was 90"
    );
    assert_eq!(
        found[1]["detail"],
        "the activity log is kept for 7 days, was 30"
    );
}

#[tokio::test]
async fn a_refused_retention_writes_no_record() {
    let api = start().await;
    let refused = api
        .send("PUT", "/api/audit/retention", Some(&json!({"days": 0})))
        .await;
    assert!(matches!(refused.status, 422 | 400));
    assert!(retention_changes(&api).await.is_empty());
}
