// SPDX-License-Identifier: GPL-3.0-or-later
//! `GET` and `PUT /api/first-run`: the flag lives in the global settings document and survives
//! the Settings screen's own saves.

use std::sync::Arc;

use puddle_api::wire::DevCertificate;
use puddle_api::{FakeDevCert, SettingsRepo};
use serde_json::json;

use crate::common::{START_MS, start, start_with_dev_cert};

#[tokio::test]
async fn a_fresh_install_has_not_been_through_the_flow_and_reading_writes_nothing() {
    let api = start().await;
    let reply = api.get("/api/first-run").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({"completed": false, "completed_at": null, "dev_certificate": "not_checked"})
    );
    assert_eq!(api.settings.load_global().unwrap(), None);
    api.running.shutdown().await;
}

#[tokio::test]
async fn finishing_the_flow_stamps_the_time_and_is_kept_in_the_settings_document() {
    let api = start().await;
    let reply = api
        .send("PUT", "/api/first-run", Some(&json!({"completed": true})))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({"completed": true, "completed_at": START_MS, "dev_certificate": "not_checked"})
    );
    assert_eq!(
        api.settings.load_global().unwrap().unwrap()["first_run"],
        json!({"completed_at": START_MS})
    );
    assert_eq!(api.get("/api/first-run").await.json()["completed"], true);
    api.running.shutdown().await;
}

#[tokio::test]
async fn finishing_twice_keeps_the_first_time() {
    let api = start().await;
    let done = json!({"completed": true});
    api.send("PUT", "/api/first-run", Some(&done)).await;
    api.clock.advance(60_000);
    let reply = api.send("PUT", "/api/first-run", Some(&done)).await;
    assert_eq!(reply.json()["completed_at"], START_MS);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_flow_can_be_opened_again() {
    let api = start().await;
    api.send("PUT", "/api/first-run", Some(&json!({"completed": true})))
        .await;
    let reply = api
        .send("PUT", "/api/first-run", Some(&json!({"completed": false})))
        .await;
    assert_eq!(
        reply.json(),
        json!({"completed": false, "completed_at": null, "dev_certificate": "not_checked"})
    );
    // Opening it again when it never ran changes nothing and writes nothing new.
    let again = api
        .send("PUT", "/api/first-run", Some(&json!({"completed": false})))
        .await;
    assert_eq!(again.json()["completed"], false);
    api.running.shutdown().await;
}

#[tokio::test]
async fn saving_other_settings_does_not_open_the_flow_again() {
    let api = start().await;
    api.send("PUT", "/api/first-run", Some(&json!({"completed": true})))
        .await;
    let reply = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({"ui": {"theme": "dark"}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(api.get("/api/first-run").await.json()["completed"], true);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_body_that_is_not_the_request_is_refused() {
    let api = start().await;
    for body in [
        json!({}),
        json!({"completed": "yes"}),
        json!({"completed": true, "extra": 1}),
    ] {
        let reply = api.send("PUT", "/api/first-run", Some(&body)).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
    }
    assert_eq!(api.get("/api/first-run").await.json()["completed"], false);
    api.running.shutdown().await;
}

#[tokio::test]
async fn settings_from_a_newer_puddle_are_left_alone() {
    let api = start().await;
    api.settings
        .save_global(json!({"schema_version": 99}))
        .unwrap();
    let reply = api.get("/api/first-run").await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.error(), "newer_settings");
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_certificates_step_reports_where_the_development_certificate_stands() {
    let cases = [
        (DevCertificate::NotChecked, "not_checked"),
        (DevCertificate::ReusingExisting, "reusing_existing"),
        (DevCertificate::Trusted, "trusted"),
        (DevCertificate::NeedsTrust, "needs_trust"),
        (DevCertificate::TrustDeclined, "trust_declined"),
        (DevCertificate::TrustFailed, "trust_failed"),
        (DevCertificate::Unsupported, "unsupported"),
    ];
    let fake = Arc::new(FakeDevCert::new(DevCertificate::NotChecked));
    let api = start_with_dev_cert(fake.clone()).await;
    for (state, wire) in cases {
        fake.set(state);
        let reply = api.get("/api/first-run").await;
        assert_eq!(reply.json()["dev_certificate"], wire);
        // Every answer to a PUT carries the same state, including a no-op one.
        for completed in [true, true, false] {
            let reply = api
                .send(
                    "PUT",
                    "/api/first-run",
                    Some(&json!({"completed": completed})),
                )
                .await;
            assert_eq!(reply.status, 200, "{}", reply.body);
            assert_eq!(reply.json()["dev_certificate"], wire);
        }
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn without_a_certificate_service_trusting_answers_not_checked() {
    let api = start().await;
    let reply = api.send("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({"dev_certificate": "not_checked", "detail": null})
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn trusting_answers_where_the_certificate_stands_and_the_next_read_agrees() {
    let fake = Arc::new(FakeDevCert::new(DevCertificate::NeedsTrust));
    let api = start_with_dev_cert(fake.clone()).await;

    let reply = api.send("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({"dev_certificate": "trusted", "detail": null})
    );
    assert_eq!(
        api.get("/api/first-run").await.json()["dev_certificate"],
        "trusted"
    );

    fake.set(DevCertificate::NeedsTrust);
    fake.set_trust_answer(DevCertificate::TrustDeclined, None);
    let reply = api.send("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(
        reply.json(),
        json!({"dev_certificate": "trust_declined", "detail": null})
    );

    fake.set_trust_answer(DevCertificate::TrustFailed, Some("os error 5".to_owned()));
    let reply = api.send("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(
        reply.json(),
        json!({"dev_certificate": "trust_failed", "detail": "os error 5"})
    );
    assert_eq!(fake.trust_calls(), 3);
    api.running.shutdown().await;
}
