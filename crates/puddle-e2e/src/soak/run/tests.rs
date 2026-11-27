// SPDX-License-Identifier: AGPL-3.0-or-later
use axum::Router;
use axum::routing::post;
use tokio::net::TcpListener;

use super::*;

#[tokio::test]
async fn a_call_sends_the_method_the_token_and_the_body() {
    let app = Router::new().route(
        "/api/x",
        post(|headers: http::HeaderMap, body: String| async move {
            let auth = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            format!("{{\"auth\":\"{auth}\",\"body\":{body}}}")
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let api = Target {
        addr: addr.to_string(),
        token: Some("t0k".to_owned()),
    };
    let (code, body) = call_json(&api, "POST", "/api/x", Some(&json!({"a": 1}))).await;
    assert_eq!(code, 200);
    assert_eq!(body, json!({"auth": "Bearer t0k", "body": {"a": 1}}));
    let (code, _) = call_json(&api, "GET", "/api/x", None).await;
    assert_eq!(code, 405);
}

#[tokio::test]
async fn a_call_to_nothing_is_status_0_with_the_error() {
    let api = Target {
        addr: "127.0.0.1:1".to_owned(),
        token: None,
    };
    let (code, body) = call_json(&api, "GET", "/", None).await;
    assert_eq!(code, 0);
    assert!(body["error"].is_string(), "{body}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_processes_below_a_process_are_found() {
    let mut child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let below = descendants(std::process::id());
    let pid = child.id();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(below.contains(&pid), "{pid} not in {below:?}");
    assert!(!below.contains(&std::process::id()));
}

#[test]
fn each_workspace_has_its_scripts_tag() {
    assert_eq!(workload_tag("qa-soak-build"), "build");
    assert_eq!(workload_tag("qa-soak-js"), "js");
    assert_eq!(workload_tag("qa-soak-agent"), "agent");
    assert_eq!(workload_tag("qa-soak-web"), "web");
}

#[test]
fn the_summary_reads_each_hour_and_the_end() {
    let mut s = Series::default();
    for h in 0..=9_u32 {
        s.push(
            1000 + u64::from(h) * 60,
            "proc.puddle.1.rss_kb",
            f64::from(h),
        );
    }
    let sched = Schedule {
        start: 1000,
        active_secs: 480,
        idle_secs: 120,
        hour: 60,
    };
    let text = summary(&s, sched, "0 finding(s)\n");
    assert!(
        text.contains("| proc.puddle.1.rss_kb | 0 | 4 | 8 | 9 |"),
        "{text}"
    );
    assert!(text.contains("0 finding(s)"), "{text}");
}

#[test]
fn a_run_without_the_memory_it_needs_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let opts = Options {
        puddle: dir.path().join("no-program"),
        runtime: None,
        agent: None,
        out: dir.path().to_owned(),
        profile: super::super::plan::SMOKE,
        scripts: None,
        ui: None,
        port: 0,
        min_ram_gib: 1 << 20,
        min_disk_gib: 0,
    };
    let err = preflight(&opts).unwrap_err();
    assert!(err.starts_with("BLOCKED"), "{err}");
    let fine = Options {
        min_ram_gib: 0,
        ..opts
    };
    assert_eq!(preflight(&fine), Ok(()));
}
