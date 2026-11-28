// SPDX-License-Identifier: AGPL-3.0-or-later
//! Built-in scenarios: they start, they are stable, reset and the connection file.

use super::*;

#[tokio::test]
async fn every_built_in_scenario_starts_with_its_seeded_data() {
    // What a scenario seeds beyond nothing: (name, pending requests, at least this many rules).
    // A built-in that is not listed must start with neither.
    let seeded = [
        ("default", 1, 0),
        ("lived-in", 8, 6),
        ("company-network", 2, 0),
        ("environment", 0, 1),
        ("request-rules", 0, 1),
    ];
    let names = built_in_names();
    for (name, ..) in seeded {
        assert!(names.contains(&name), "{name} is not a built-in scenario");
    }
    for name in names {
        let (pending, rules_min) = seeded
            .iter()
            .find(|(seeded_name, ..)| *seeded_name == name)
            .map_or((0, 0), |&(_, pending, rules_min)| (pending, rules_min));
        let run = start(name).await;
        let state = run.state().await;
        assert_eq!(state["scenario"], name);
        assert_eq!(state["pending"], pending, "{name}");
        let rules = run.get("/api/rules").await.json();
        assert!(
            rules["rules"].as_array().unwrap().len() >= rules_min,
            "{name}: {rules}"
        );
        assert_eq!(run.get("/api/pending").await.status, 200);
        assert_eq!(run.get("/api/audit").await.status, 200);
        run.fixture.shutdown().await;
    }
}

/// The binary's start-up runs on its main thread inside `#[tokio::main]`, whose stack `build.rs`
/// links at 8 MiB on Windows (the default 1 MiB overflowed in a debug build, intermittently).
/// Here each built-in scenario starts the same way on a thread with half of that, so the binary
/// keeps at least twice what its start-up needs and a scenario that grows shows up here, not as an
/// abort in the UI tests.
#[test]
fn every_built_in_scenario_starts_on_half_the_programs_main_thread_stack() {
    let stack = puddle_e2e::ui_fixture::stack::linked_stack_bytes() / 2;
    for name in built_in_names() {
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn(move || {
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let run = start(name).await;
                        run.fixture.shutdown().await;
                    });
            })
            .unwrap()
            .join()
            .unwrap_or_else(|_| panic!("scenario {name} did not start on {stack} bytes of stack"));
    }
}

#[tokio::test]
async fn the_default_scenario_is_the_one_pending_request_the_ui_gates_count_on() {
    let run = start("default").await;
    let pending = run.get("/api/pending").await.json();
    let rows = pending["requests"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{pending}");
    assert_eq!(rows[0]["workspace"], "demo");
    assert_eq!(rows[0]["host"], "registry.example.org");
    assert_eq!(run.get("/api/rules").await.json()["rules"], json!([]));
}

#[tokio::test]
async fn the_same_scenario_gives_byte_identical_responses() {
    let paths = [
        "/api/pending",
        "/api/inbox",
        "/api/rules",
        "/api/audit?limit=500",
        "/api/workspaces",
        "/api/workspaces/docs-site/delete-check",
    ];
    let a = start("lived-in").await;
    let b = start("lived-in").await;
    for path in paths {
        let (x, y) = (a.get(path).await, b.get(path).await);
        assert_eq!(x.status, 200, "{path}");
        assert_eq!(x.body, y.body, "{path}");
        assert!(x.body.len() > 20, "{path}: {}", x.body);
    }
}

#[tokio::test]
async fn reset_starts_over_from_the_scenario_or_another_one() {
    let run = start("default").await;
    run.control(
        "POST",
        "/control/step",
        Some(&json!({"do": "request", "workspace": "demo", "host": "more.example.org"})),
    )
    .await;
    assert_eq!(run.state().await["pending"], 2);
    assert_eq!(
        run.control("POST", "/control/reset", None).await.status,
        204
    );
    assert_eq!(run.state().await["pending"], 1);
    let other = run
        .control(
            "POST",
            "/control/reset",
            Some(&json!({"scenario": "lived-in"})),
        )
        .await;
    assert_eq!(other.status, 204);
    assert_eq!(run.state().await["scenario"], "lived-in");
    let unknown = run
        .control(
            "POST",
            "/control/reset",
            Some(&json!({"scenario": "no-such"})),
        )
        .await;
    assert_eq!(unknown.status, 422);
    // The API answers on the same port and token throughout.
    assert_eq!(run.get("/api/pending").await.status, 200);
}

#[tokio::test]
async fn a_scenario_with_a_typo_or_a_bad_value_fails_to_start() {
    assert!(serde_json::from_value::<Scenario>(json!({"rulez": []})).is_err());
    let bad_host: Scenario =
        serde_json::from_value(json!({"requests": [{"workspace": "a", "host": "bad host!"}]}))
            .unwrap();
    let err = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: None,
        scenario: bad_host,
    })
    .await
    .err()
    .unwrap();
    assert!(err.contains("bad host!"), "{err}");
    let bad_reason: Scenario = serde_json::from_value(json!({"connections": [
        {"workspace": "a", "host": "x.example.org", "decision": "blocked", "reason": "mystery"}
    ]}))
    .unwrap();
    assert!(
        Fixture::start(FixtureOptions {
            port: 0,
            connection_file: None,
            scenario: bad_reason
        })
        .await
        .err()
        .unwrap()
        .contains("mystery")
    );
    let missing = load_scenario("/no/such/scenario.json").unwrap_err();
    assert!(missing.contains("default, empty, lived-in"), "{missing}");
}

#[tokio::test]
async fn the_connection_file_is_written_for_the_dev_proxy_and_playwright() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("sub").join("connection.json");
    let fixture = Fixture::start(FixtureOptions {
        port: 0,
        connection_file: Some(file.clone()),
        scenario: load_scenario("empty").unwrap(),
    })
    .await
    .unwrap();
    let written: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["version"], 1);
    let info = fixture.connection_info().await.unwrap();
    assert_eq!(written["url"], info.url);
    assert_eq!(written["token"], info.token.expose());
    // A restart rewrites it (same token, same port).
    fixture.restart().await.unwrap();
    let again: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(again, written);
}

#[tokio::test]
async fn only_the_first_run_scenario_still_has_the_first_run_flow_to_show() {
    for name in ["default", "empty", "lived-in", "corporate-network"] {
        let run = start(name).await;
        let state = run.get("/api/first-run").await.json();
        assert_eq!(state["completed"], true, "{name}: {state}");
        assert_eq!(state["completed_at"], run.fixture.now_ms().await);
        run.fixture.shutdown().await;
    }
    let run = start("first-run").await;
    let state = run.get("/api/first-run").await.json();
    assert_eq!(state["completed"], false, "{state}");
    assert_eq!(state["completed_at"], Value::Null);
    run.fixture.shutdown().await;
}

#[tokio::test]
async fn a_scenario_keeps_the_settings_it_seeds_next_to_the_finished_flow() {
    let text = json!({
        "name": "themed",
        "settings": {"global": {"schema_version": 2, "ui": {"theme": "dark"}}}
    })
    .to_string();
    let run = start_scenario(serde_json::from_str(&text).unwrap()).await;
    let settings = run.get("/api/settings").await.json();
    assert_eq!(settings["ui"]["theme"], "dark");
    assert_eq!(run.get("/api/first-run").await.json()["completed"], true);
    run.fixture.shutdown().await;
}

/// The program itself, started as a user starts it: `--help` prints the usage and exits 0, and a
/// bad option exits 2 with the reason on stderr (the same path the stack-size guard reports on).
#[test]
fn the_program_prints_its_help_and_exits_2_on_a_bad_option() {
    let program = env!("CARGO_BIN_EXE_puddle-ui-fixture");
    let help = std::process::Command::new(program)
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success(), "{help:?}");
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("usage: puddle-ui-fixture"),
        "{help:?}"
    );
    let bad = std::process::Command::new(program)
        .arg("--no-such-option")
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(2), "{bad:?}");
    assert!(
        String::from_utf8_lossy(&bad.stderr).starts_with("puddle-ui-fixture: "),
        "{bad:?}"
    );
}
