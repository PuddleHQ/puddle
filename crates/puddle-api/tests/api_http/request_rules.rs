// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on the method and path of a request over real HTTP, against a real store.

use serde_json::{Value, json};

use crate::common::start;

#[tokio::test]
async fn a_rule_is_made_listed_by_scope_and_deleted() {
    let api = start().await;
    assert_eq!(
        api.get("/api/request-rules").await.json(),
        json!({"rules": []})
    );

    let made = api
        .send(
            "POST",
            "/api/request-rules",
            Some(&json!({
                "workspace": null,
                "host": "API.Example.com.",
                "methods": ["delete", "Put", "DELETE"],
                "path": "/repos/*/issues/**"
            })),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    let rule = made.json();
    assert_eq!(rule["id"], 1);
    assert_eq!(rule["workspace"], Value::Null);
    assert_eq!(rule["host"], "api.example.com", "the host is normalised");
    assert_eq!(rule["methods"], json!(["DELETE", "PUT"]));
    assert_eq!(rule["path"], "/repos/*/issues/**");
    assert_eq!(rule["created_by"], "api");

    let own = api
        .send(
            "POST",
            "/api/request-rules",
            Some(&json!({"workspace": "shop", "host": "*.example.org", "path": "/"})),
        )
        .await;
    assert_eq!(own.status, 201, "{}", own.body);
    assert_eq!(
        own.json()["methods"],
        json!([]),
        "no method is every method"
    );
    api.send(
        "POST",
        "/api/request-rules",
        Some(&json!({"workspace": "other", "host": "x.example.net", "path": "/x"})),
    )
    .await;

    let all = api.get("/api/request-rules").await.json();
    assert_eq!(all["rules"].as_array().unwrap().len(), 3);
    let shop = api.get("/api/request-rules?workspace=shop").await.json();
    let ids: Vec<i64> = shop["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [1, 2], "the global rules and its own");

    let gone = api.send("DELETE", "/api/request-rules/1", None).await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert_eq!(gone.json()["id"], 1);
    assert_eq!(
        api.send("DELETE", "/api/request-rules/1", None)
            .await
            .status,
        404
    );

    let audit = api.get("/api/audit?type=request_rule_deleted").await.json();
    assert_eq!(
        audit["entries"][0]["record"]["rule"]["host"],
        "api.example.com"
    );
    assert_eq!(audit["entries"][0]["record"]["actor"], "api");
}

#[tokio::test]
async fn a_rule_that_cannot_work_is_refused_and_nothing_is_made() {
    let api = start().await;
    let cases = [
        json!({"host": "10.0.0.1", "path": "/"}),
        json!({"host": "*.com", "path": "/"}),
        json!({"host": "not a host", "path": "/"}),
        json!({"host": "a.example.com", "path": "no-slash"}),
        json!({"host": "a.example.com", "path": "/a/**/b"}),
        json!({"host": "a.example.com", "path": "/a?x=1"}),
        json!({"host": "a.example.com", "methods": ["GE T"], "path": "/"}),
        json!({"host": "a.example.com", "methods": [""], "path": "/"}),
        json!({"host": "a.example.com", "workspace": "Not A Name!", "path": "/"}),
        json!({"host": "a.example.com", "path": "/", "extra": 1}),
    ];
    for body in cases {
        let reply = api.send("POST", "/api/request-rules", Some(&body)).await;
        assert!(
            reply.status == 422 || reply.status == 400,
            "{body}: {} {}",
            reply.status,
            reply.body
        );
    }
    assert_eq!(
        api.get("/api/request-rules").await.json(),
        json!({"rules": []})
    );
    let bad_query = api
        .get("/api/request-rules?workspace=Not%20A%20Name!")
        .await;
    assert_eq!(bad_query.status, 422, "{}", bad_query.body);
}
