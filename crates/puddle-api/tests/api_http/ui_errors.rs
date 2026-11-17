// SPDX-License-Identifier: AGPL-3.0-or-later
//! A page reports an error no code expected: it reaches the host's log scrubbed, Settings lists
//! it, and a page in a loop of errors is held to a rate.

use puddle_test_kit::capture_logs;
use serde_json::{Value, json};

use crate::common::start;

const TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn thrown(message: &str) -> Value {
    json!({
        "kind": "page",
        "message": message,
        "stack": "  at render (http://127.0.0.1:4100/_app/immutable/nodes/31.js:9:7650)\n  at https://me:hunter2@h.example/x.js?access_token=abc:1:2",
        "route": "/workspaces/abc/terminal?ticket=xyz",
        "build": "1700000000000",
    })
}

#[tokio::test]
async fn a_thrown_error_reaches_the_log_scrubbed_and_settings_lists_it() {
    let (logs, _guard) = capture_logs();
    let api = start().await;
    let message = format!("TypeError: Bearer {TOKEN} was refused");
    let reply = api
        .send("POST", "/api/ui-errors", Some(&thrown(&message)))
        .await;
    assert_eq!(reply.status, 204, "{}", reply.body);

    let log = logs.text();
    let line = log
        .lines()
        .find(|l| l.contains("a page hit an unexpected error"))
        .unwrap_or_else(|| panic!("no log line in {log}"));
    assert!(line.contains("WARN"), "{line}");
    assert!(
        line.contains("TypeError: Bearer [redacted] was refused"),
        "{line}"
    );
    assert!(line.contains("route=/workspaces/abc/terminal "), "{line}");
    assert!(line.contains("build=1700000000000"), "{line}");
    assert!(
        line.contains(r"at https://h.example/x.js:1:2"),
        "the stack is in the line: {line}"
    );
    for secret in [TOKEN, "hunter2", "access_token", "ticket=xyz"] {
        assert!(!log.contains(secret), "{secret} reached the log: {log}");
    }

    let listed = api.send("GET", "/api/ui-errors", None).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let list = listed.json();
    assert_eq!(list["dropped"], 0);
    let errors = list["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["kind"], "page");
    assert_eq!(
        errors[0]["message"],
        "TypeError: Bearer [redacted] was refused"
    );
    assert_eq!(errors[0]["route"], "/workspaces/abc/terminal");
    assert_eq!(errors[0]["build"], "1700000000000");
    assert_eq!(errors[0]["at"], crate::common::START_MS);
    assert_eq!(
        errors[0]["stack"],
        "  at render (http://127.0.0.1:4100/_app/immutable/nodes/31.js:9:7650)\n  at https://h.example/x.js:1:2"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_page_in_a_loop_of_errors_is_held_to_a_rate_and_logged_once_for_it() {
    let (logs, _guard) = capture_logs();
    let api = start().await;
    for i in 0..10 {
        let reply = api
            .send("POST", "/api/ui-errors", Some(&thrown(&format!("e{i}"))))
            .await;
        assert_eq!(reply.status, 204, "{i}");
    }
    for _ in 0..3 {
        let reply = api
            .send("POST", "/api/ui-errors", Some(&thrown("over")))
            .await;
        assert_eq!((reply.status, reply.error()), (429, "rate_limited".into()));
        assert_eq!(reply.header("retry-after"), Some("60"));
    }
    let log = logs.text();
    assert_eq!(
        log.matches("a page hit an unexpected error").count(),
        10,
        "{log}"
    );
    assert_eq!(
        log.matches("pages reported too many errors").count(),
        1,
        "{log}"
    );
    let list = api.send("GET", "/api/ui-errors", None).await.json();
    assert_eq!(list["dropped"], 3);
    assert_eq!(list["errors"].as_array().unwrap().len(), 10);

    // A minute later the page is heard again.
    api.clock.advance(60_000);
    let reply = api
        .send("POST", "/api/ui-errors", Some(&thrown("later")))
        .await;
    assert_eq!(reply.status, 204);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_report_that_is_not_one_is_refused() {
    let api = start().await;
    let mut extra = thrown("x");
    extra["token"] = json!("y");
    let mut kind = thrown("x");
    kind["kind"] = json!("toaster");
    for bad in [extra, kind, json!({"kind": "page"})] {
        let reply = api.send("POST", "/api/ui-errors", Some(&bad)).await;
        assert_eq!(reply.status, 422, "{bad}");
    }
    // A body over the API's limit never reaches the handler.
    let huge = thrown(&"x".repeat(70 * 1024));
    let reply = api.send("POST", "/api/ui-errors", Some(&huge)).await;
    assert_eq!(reply.status, 413);
    api.running.shutdown().await;
}
