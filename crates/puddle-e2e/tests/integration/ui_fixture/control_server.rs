// SPDX-License-Identifier: AGPL-3.0-or-later
//! The control server and scripted steps: events, scripts, spread, restart and the drop folder.

use super::*;

#[tokio::test]
async fn emit_puts_an_event_on_the_stream_and_any_event_json_is_accepted() {
    let run = start("empty").await;
    let mut stream = run.events().await;
    // The stream is subscribed once the hub has a receiver; emit until it shows up.
    let event = json!({"type": "oom_kill", "workspace": "web-shop", "pid": 7, "process": "node"});
    let sent = run.control("POST", "/control/emit", Some(&event)).await;
    assert_eq!(sent.status, 204);
    let seen = read_until(&mut stream, |s| s.contains("oom_kill")).await;
    assert!(seen.contains("\"pid\":7"), "{seen}");

    let bad = run
        .control("POST", "/control/emit", Some(&json!({"type": "nope"})))
        .await;
    assert!(bad.status == 422 || bad.status == 400, "{}", bad.status);
}

#[tokio::test]
async fn scripts_run_in_order_and_an_unknown_one_is_404() {
    let run = start("lived-in").await;
    let before = run.state().await["pending"].as_u64().unwrap();
    let ran = run.control("POST", "/control/script/arrivals", None).await;
    assert_eq!(ran.status, 204);
    assert_eq!(run.state().await["pending"].as_u64().unwrap(), before + 3);
    let missing = run.control("POST", "/control/script/nope", None).await;
    assert_eq!(missing.status, 404);
    // A script with a bad step reports it.
    let bad = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "request", "workspace": "Not A Name", "host": "x.example.org"})),
        )
        .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
}

#[tokio::test]
async fn a_spread_adds_that_many_pending_requests_and_an_orphan_volume_is_listed() {
    let run = start("lived-in").await;
    let before = run.state().await["pending"].as_u64().unwrap();
    let spread = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "spread", "count": 7})),
        )
        .await;
    assert_eq!(spread.status, 204, "{}", spread.body);
    assert_eq!(run.state().await["pending"].as_u64().unwrap(), before + 7);

    let orphan = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "orphan_volume", "id": "bare"})),
        )
        .await;
    assert_eq!(orphan.status, 204, "{}", orphan.body);
    let listed = run.get("/api/orphan-volumes").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(listed.body.contains("bare"), "{}", listed.body);

    let bad = run
        .control(
            "POST",
            "/control/step",
            Some(&json!({"do": "orphan_volume", "id": "Not A Valid Id!"})),
        )
        .await;
    assert_eq!(bad.status, 422, "{}", bad.body);
}

#[tokio::test]
async fn restart_ends_open_streams_and_keeps_the_data_on_the_same_port() {
    let run = start("lived-in").await;
    let mut stream = run.events().await;
    let before = run.get("/api/pending").await.body;
    let restarted = run.control("POST", "/control/restart", None).await;
    assert_eq!(restarted.status, 204);
    // The old stream ends (read returns 0 eventually).
    let mut buf = [0u8; 1024];
    tokio::time::timeout(STEP, async {
        loop {
            if stream.read(&mut buf).await.unwrap_or(0) == 0 {
                break;
            }
        }
    })
    .await
    .expect("the stream ended");
    assert_eq!(run.fixture.addr().await.unwrap(), run.api);
    assert_eq!(run.get("/api/pending").await.body, before);
}

#[tokio::test]
async fn a_dropped_file_makes_requests_arrive_in_the_old_serve_ui_format() {
    let run = start("empty").await;
    let dir = tempfile::tempdir().unwrap();
    let _watch = watch_drop_dir(run.fixture.clone(), dir.path().to_owned());
    let file = dir.path().join("a.json");
    std::fs::write(
        dir.path().join("a.tmp"),
        json!([
            {"workspace": "demo", "host": "one.example.org", "repeat": 2},
            {"workspace": "demo", "count": 3, "domain": "many.example.org"}
        ])
        .to_string(),
    )
    .unwrap();
    std::fs::rename(dir.path().join("a.tmp"), &file).unwrap();
    puddle_test_kit::eventually("the drop file was taken", STEP, || !file.exists()).await;
    assert_eq!(run.state().await["pending"], 4);
    let rows = run.get("/api/pending").await.json();
    let attempts: Vec<i64> = rows["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["host"] == "one.example.org")
        .map(|r| r["attempts"].as_i64().unwrap())
        .collect();
    assert_eq!(attempts, [2]);
}

#[tokio::test]
async fn a_dropped_file_is_gone_only_once_its_steps_are_applied() {
    let run = start("empty").await;
    let dir = tempfile::tempdir().unwrap();
    let _watch = watch_drop_dir(run.fixture.clone(), dir.path().to_owned());
    let file = dir.path().join("a.json");
    // The wait step holds the file's work up for a real 300 ms, so a file that is deleted
    // before its steps run is seen gone while nothing is applied yet.
    std::fs::write(
        dir.path().join("a.tmp"),
        json!([
            {"do": "wait", "ms": 300},
            {"workspace": "demo", "host": "one.example.org"}
        ])
        .to_string(),
    )
    .unwrap();
    std::fs::rename(dir.path().join("a.tmp"), &file).unwrap();
    puddle_test_kit::eventually("the drop file was taken", STEP, || !file.exists()).await;
    assert_eq!(run.state().await["pending"], 1);
}

#[test]
fn drop_items_without_do_are_requests_or_bulks_and_others_are_refused() {
    assert!(matches!(
        step_from_drop(json!({"workspace": "a", "host": "x.example.org"})),
        Ok(Step::Request(_))
    ));
    assert!(matches!(
        step_from_drop(json!({"workspace": "a", "count": 2})),
        Ok(Step::Bulk { count: 2, .. })
    ));
    assert!(step_from_drop(json!({"workspace": "a", "hots": "x"})).is_err());
    assert!(step_from_drop(json!(7)).is_err());
}
