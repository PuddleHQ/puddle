// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

#[test]
fn a_ready_page_gives_its_result_and_its_console() {
    let outcome = parse_reply(
        "{\"ok\":true,\"result\":{\"n\":3},\"console\":[\"log: hi\"],\"pageErrors\":[]}",
    )
    .expect("answer");
    assert_eq!(outcome.result, json!({"n": 3}));
    assert_eq!(outcome.console, ["log: hi"]);
    assert_eq!(outcome.page_errors, Vec::<String>::new());
}

#[test]
fn a_failed_page_names_the_reason_the_console_and_the_page_errors() {
    let err = parse_reply(
            "{\"ok\":false,\"error\":\"waiting failed\",\"console\":[\"error: boom\"],\"pageErrors\":[\"Error: x\"]}",
        )
        .expect_err("not ok");
    let text = err.to_string();
    assert!(text.contains("waiting failed"), "{text}");
    assert!(text.contains("error: boom"), "{text}");
    assert!(text.contains("Error: x"), "{text}");
}

#[test]
fn an_unreadable_answer_is_an_error_not_a_pass() {
    let err = parse_reply("{not json").expect_err("bad json");
    assert!(err.to_string().contains("unreadable"), "{err}");
}
