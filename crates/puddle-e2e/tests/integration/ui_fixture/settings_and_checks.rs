// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings, system checks, the certificate, environment and listening ports.

use super::*;

#[tokio::test]
async fn settings_in_a_scenario_are_served_with_their_unknown_fields_listed() {
    let scenario: Scenario = serde_json::from_value(json!({
        "name": "settings",
        "settings": {
            "global": {"schema_version": 1, "workspace_defaults": {"zoom_hotkeys": false}},
            "workspaces": {"web-shop": {"schema_version": 1}}
        }
    }))
    .unwrap();
    let run = start_scenario(scenario).await;
    let global = run.get("/api/settings").await;
    assert_eq!(global.status, 200, "{}", global.body);
    assert_eq!(global.json()["unknown_fields"], json!([]));
    assert_eq!(
        run.get("/api/settings/workspaces/web-shop").await.status,
        200
    );
}

#[tokio::test]
async fn the_system_check_is_healthy_until_a_scenario_or_a_step_says_otherwise() {
    let run = start("empty").await;
    let healthy = run.get("/api/doctor").await;
    assert_eq!(healthy.status, 200, "{}", healthy.body);
    assert_eq!(healthy.json()["ok"], true);

    let mut broken = healthy.json();
    broken["ok"] = json!(false);
    broken["checks"][1]["status"] = json!("fail");
    broken["checks"][1]["summary"] = json!("KVM is missing");
    let mut doctor_step = broken.clone();
    doctor_step["do"] = json!("doctor");
    let step = run
        .control("POST", "/control/step", Some(&doctor_step))
        .await;
    assert_eq!(step.status, 204, "{}", step.body);
    let after = run.get("/api/doctor").await.json();
    assert_eq!(after["ok"], false);
    assert_eq!(after["checks"][1]["summary"], "KVM is missing");

    // A scenario can seed the report too, and a typo in it fails at start.
    let seeded = json!({"name": "broken", "doctor": broken}).to_string();
    let run2 = start_scenario(serde_json::from_str(&seeded).unwrap()).await;
    assert_eq!(run2.get("/api/doctor").await.json()["ok"], false);
    let mut typo = broken;
    typo["oks"] = json!(true);
    let err = serde_json::from_str::<Scenario>(&json!({"doctor": typo}).to_string())
        .unwrap_err()
        .to_string();
    assert!(err.contains("oks"), "{err}");
}

#[tokio::test]
async fn the_certificate_is_not_checked_until_a_scenario_or_a_step_sets_it_and_trusting_follows_the_step()
 {
    let run = start("first-run").await;
    let dev_certificate = |reply: Reply| reply.json()["dev_certificate"].clone();
    assert_eq!(
        dev_certificate(run.get("/api/first-run").await),
        "not_checked"
    );

    let step = json!({"do": "dev_certificate", "state": "needs_trust"});
    assert_eq!(
        run.control("POST", "/control/step", Some(&step))
            .await
            .status,
        204
    );
    assert_eq!(
        dev_certificate(run.get("/api/first-run").await),
        "needs_trust"
    );
    // The default answer is a yes.
    let answer = run.api("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(
        answer.status,
        200,
        "trust answered with {} body bytes",
        answer.body.len()
    );
    assert_eq!(
        answer.json(),
        json!({"dev_certificate": "trusted", "detail": null, "waiting": false})
    );
    assert_eq!(dev_certificate(run.get("/api/first-run").await), "trusted");

    let refused = json!({
        "do": "dev_certificate", "state": "needs_trust",
        "trust_answer": "trust_failed", "detail": "os error 5"
    });
    run.control("POST", "/control/step", Some(&refused)).await;
    let failed = run.api("POST", "/api/dev-certificate/trust", None).await;
    assert_eq!(
        failed.json(),
        json!({"dev_certificate": "trust_failed", "detail": "os error 5", "waiting": false})
    );

    let seeded = json!({"name": "untrusted", "dev_certificate": "needs_trust"}).to_string();
    let run2 = start_scenario(serde_json::from_str(&seeded).unwrap()).await;
    assert_eq!(
        dev_certificate(run2.get("/api/first-run").await),
        "needs_trust"
    );
    run.fixture.shutdown().await;
    run2.fixture.shutdown().await;
}

#[tokio::test]
async fn a_scenario_whose_global_settings_are_not_an_object_fails_to_start() {
    let scenario: Scenario =
        serde_json::from_str(r#"{"name": "bad", "settings": {"global": []}}"#).unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario,
    })
    .await
    .err()
    .expect("a start with unusable settings is refused");
    assert!(err.contains("not an object"), "{err}");
}

#[tokio::test]
async fn the_environment_scenario_seeds_variables_secrets_shell_init_and_listening_ports() {
    let run = start("environment").await;

    let global = run.get("/api/env").await.json();
    let names: Vec<_> = global["variables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["EDITOR", "GHE_TOKEN"]);

    let shop = run.get("/api/workspaces/web-shop/env").await.json();
    let rows = shop["variables"].as_array().unwrap();
    let secret = rows.iter().find(|v| v["name"] == "NPM_TOKEN").unwrap();
    assert_eq!(secret["kind"], "secret");
    assert_eq!(secret["value"], Value::Null);
    assert_eq!(
        secret["hosts"],
        json!(["*.npmjs.org", "registry.npmjs.org"])
    );
    assert!(
        !shop.to_string().contains("npm-seed-value"),
        "a secret's value is never in a response: {shop}"
    );
    assert!(rows.iter().any(|v| v["scope"] == "global"), "{shop}");

    let init = run.get("/api/workspaces/web-shop/shell-init").await.json();
    assert_eq!(init["content"], "export PAGER=less\n");
    let none = run.get("/api/workspaces/docs-site/shell-init").await.json();
    assert_eq!(none["saved"], false);

    let ports = run.get("/api/workspaces/web-shop/ports").await.json();
    let listening: Vec<_> = ports["ports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["guest_port"].as_u64().unwrap())
        .collect();
    assert_eq!(listening, [3000, 5432]);
}

#[tokio::test]
async fn steps_change_what_listens_and_what_the_shell_init_check_says() {
    let run = start("environment").await;
    let step = |body: Value| {
        let run = &run;
        async move { run.control("POST", "/control/step", Some(&body)).await }
    };
    let mut events = run.events().await;
    assert_eq!(
        step(json!({"do": "listening", "workspace": "web-shop", "ports": [8080]}))
            .await
            .status,
        204
    );
    let seen = read_until(&mut events, |text| text.contains("workspace_ports_changed")).await;
    assert!(seen.contains("web-shop"), "{seen}");
    let ports = run.get("/api/workspaces/web-shop/ports").await.json();
    assert_eq!(ports["ports"].as_array().unwrap().len(), 1);
    assert_eq!(ports["ports"][0]["guest_port"], 8080);

    assert_eq!(
        step(json!({"do": "learned", "workspace": "web-shop", "host": "pinned.example", "expires_in_secs": 90}))
            .await
            .status,
        204
    );
    let seen = read_until(&mut events, |text| text.contains("pass_through_learned")).await;
    assert!(seen.contains("pinned.example"), "{seen}");
    let learned = run
        .get("/api/workspaces/web-shop/interception/learned")
        .await
        .json();
    let hosts = learned["hosts"].as_array().unwrap();
    assert_eq!(hosts.len(), 1, "{learned}");
    assert_eq!(
        (
            hosts[0]["host"].as_str(),
            hosts[0]["passed_through"].as_bool(),
            hosts[0]["expires_in_secs"].as_u64(),
            hosts[0]["count"].as_u64()
        ),
        (Some("pinned.example"), Some(true), Some(90), Some(1)),
        "{learned}"
    );
    assert_eq!(
        step(json!({"do": "learned", "workspace": "web-shop", "host": "other.example"}))
            .await
            .status,
        204
    );
    let learned = run
        .get("/api/workspaces/web-shop/interception/learned")
        .await
        .json();
    let default = learned["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["host"] == "other.example")
        .unwrap();
    assert_eq!(default["expires_in_secs"], 43_200, "{learned}");

    let put = |content: &str| {
        let run = &run;
        let body = json!({ "content": content });
        async move {
            run.api("PUT", "/api/workspaces/web-shop/shell-init", Some(&body))
                .await
        }
    };
    assert_eq!(put("echo hi").await.status, 200);
    assert_eq!(
        step(json!({"do": "shell_init_check", "refuses": "it printed: hi"}))
            .await
            .status,
        204
    );
    let refused = put("echo hi").await;
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert!(refused.body.contains("it printed: hi"), "{}", refused.body);
    assert_eq!(step(json!({"do": "shell_init_check"})).await.status, 204);
    assert_eq!(put("echo hi").await.status, 200);
}

#[tokio::test]
async fn a_scenario_whose_variable_is_refused_fails_to_start_and_names_it() {
    for (seed, word) in [
        (json!({"name": "1BAD"}), "1BAD"),
        (json!({"name": "PATH"}), "PATH"),
        (
            json!({"name": "TOKEN", "kind": "secret", "value": "v", "hosts": ["1.2.3.4"]}),
            "TOKEN",
        ),
        (
            json!({"name": "TOKEN", "kind": "secret", "hosts": []}),
            "TOKEN",
        ),
    ] {
        let scenario: Scenario = serde_json::from_value(json!({"env": [seed]})).unwrap();
        let err = Fixture::start(FixtureOptions {
            port: 0,
            connection_file: None,
            scenario,
        })
        .await
        .err()
        .unwrap();
        assert!(err.contains(word), "{err}");
    }
    let shell: Scenario = serde_json::from_value(json!({"workspaces": [
        {"name": "web", "shell_init": "a\u{0}b", "env": [{"name": "OK", "value": "1"}]}
    ]}))
    .unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario: shell,
    })
    .await
    .err()
    .unwrap();
    assert!(err.contains("web"), "{err}");
}
