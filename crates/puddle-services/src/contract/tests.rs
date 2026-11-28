// SPDX-License-Identifier: AGPL-3.0-or-later
//! The suite's own checks: each helper fails on the wrong answer and each case makes the calls it
//! names, so a suite that silently does nothing is caught here.

use std::cell::RefCell;

use super::*;

/// A one-workspace service that answers like the API, with its create end state set by the test.
struct Scripted {
    ends_running: bool,
    status: RefCell<&'static str>,
    calls: RefCell<Vec<String>>,
}

impl Scripted {
    fn new(ends_running: bool) -> Self {
        Self {
            ends_running,
            status: RefCell::new("created"),
            calls: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }
}

impl Subject for Scripted {
    fn call<'a>(
        &'a self,
        method: &'static str,
        path: &'a str,
        _body: Option<Value>,
    ) -> LocalBoxFuture<'a, Reply> {
        self.calls.borrow_mut().push(format!("{method} {path}"));
        let reply = |status, body| Reply { status, body };
        let answer = match (method, path) {
            ("POST", "/api/workspaces") => {
                *self.status.borrow_mut() = if self.ends_running {
                    "running"
                } else {
                    "created"
                };
                reply(202, json!({"id": "w1"}))
            }
            ("GET", "/api/workspaces/w1") => reply(
                200,
                json!({"id": "w1", "status": *self.status.borrow(), "busy": null}),
            ),
            ("POST", "/api/workspaces/w1/stop") => {
                *self.status.borrow_mut() = "stopped";
                reply(202, json!({}))
            }
            ("POST", "/api/workspaces/w1/start") => {
                *self.status.borrow_mut() = "running";
                reply(202, json!({}))
            }
            _ => reply(500, json!({"error": "internal", "message": "unscripted"})),
        };
        Box::pin(async move { answer })
    }
}

fn refusal(status: u16, code: &str, message: &str) -> Reply {
    Reply {
        status,
        body: json!({"error": code, "message": message}),
    }
}

#[test]
fn each_refusal_status_has_its_own_code() {
    assert_eq!(code_of(404), "not_found");
    assert_eq!(code_of(409), "conflict");
    assert_eq!(code_of(422), "invalid");
}

#[test]
#[should_panic(expected = "no error code is named for status 500")]
fn a_status_with_no_named_code_is_a_mistake_in_the_suite() {
    code_of(500);
}

#[test]
fn a_refusal_with_the_status_code_and_words_passes() {
    expect_refusal(
        "c",
        "s",
        &refusal(409, "conflict", "it is busy now"),
        409,
        "busy",
    );
}

#[test]
#[should_panic(expected = "the error code")]
fn a_refusal_with_another_code_fails() {
    expect_refusal("c", "s", &refusal(409, "not_found", "busy"), 409, "busy");
}

#[test]
#[should_panic(expected = "the refusal should say")]
fn a_refusal_without_the_words_fails() {
    expect_refusal("c", "s", &refusal(409, "conflict", "idle"), 409, "busy");
}

#[test]
#[should_panic(expected = "left: 404")]
fn a_refusal_with_another_status_fails() {
    expect_refusal("c", "s", &refusal(404, "not_found", "busy"), 409, "busy");
}

#[tokio::test]
async fn a_create_that_ends_running_is_stopped_and_its_id_returned() {
    let subject = Scripted::new(true);
    assert_eq!(created(&subject, "case").await, "w1");
    assert_eq!(*subject.status.borrow(), "stopped");
    assert_eq!(subject.calls().last().unwrap(), "GET /api/workspaces/w1");
}

#[tokio::test]
#[should_panic(expected = "a create ends running")]
async fn a_create_that_ends_another_way_breaks_the_contract() {
    created(&Scripted::new(false), "case").await;
}

#[tokio::test]
async fn a_stopped_workspace_is_started_again_through_the_api() {
    let subject = Scripted::new(true);
    a_stopped_workspace_starts_again(&subject).await;
    let starts = subject
        .calls()
        .iter()
        .filter(|c| *c == "POST /api/workspaces/w1/start")
        .count();
    assert_eq!(starts, 2, "{:?}", subject.calls());
    assert_eq!(*subject.status.borrow(), "running");
}

#[tokio::test]
#[should_panic(expected = "unknown")]
async fn the_suite_fails_a_service_that_answers_nothing_it_expects() {
    run(&Scripted::new(true)).await;
}
