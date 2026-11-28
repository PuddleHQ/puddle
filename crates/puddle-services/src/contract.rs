// SPDX-License-Identifier: AGPL-3.0-or-later
//! One suite of workspace-service behaviour that every implementation behind the API runs.
//!
//! The API tests run on [`crate::FakeWorkspaces`] and the host tests on the real service. Both
//! feed this suite through a [`Subject`], so a rule the real service changes and the fake does
//! not fails the fake's run, and the other way round. It speaks HTTP shapes only (paths, status
//! codes, JSON fields), never the service's types, so an implementation needs no hook for it.
//!
//! The lifecycle's rules live in [`crate::phase`] (one transition function and the actions it
//! admits), which both services call; this suite checks that each service wires them to the
//! right operation and ends every operation in the state the API promises.
#![expect(clippy::panic, reason = "a contract case fails its test by panicking")]

use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use serde_json::{Value, json};

/// How long a case waits for an operation to finish.
const SETTLE: Duration = Duration::from_secs(30);

/// One answer: the status code and the JSON body (`Null` when there is none).
#[derive(Debug, Clone)]
pub struct Reply {
    /// The HTTP status code.
    pub status: u16,
    /// The body, parsed.
    pub body: Value,
}

/// What a service under test offers the suite: the API it serves.
pub trait Subject {
    /// Sends one request to the API of the service under test.
    fn call<'a>(
        &'a self,
        method: &'static str,
        path: &'a str,
        body: Option<Value>,
    ) -> LocalBoxFuture<'a, Reply>;
}

/// A fresh workspace name, so the cases never meet each other's workspaces.
fn name(case: &str) -> String {
    format!("contract-{case}")
}

fn field<'a>(reply: &'a Reply, key: &str) -> &'a Value {
    reply.body.get(key).unwrap_or(&Value::Null)
}

fn text(reply: &Reply) -> String {
    reply.body.to_string()
}

fn expect_status(case: &str, step: &str, reply: &Reply, status: u16) {
    assert_eq!(reply.status, status, "{case}: {step}: {}", text(reply));
}

/// The stable error code each refusal status carries.
fn code_of(status: u16) -> &'static str {
    match status {
        404 => "not_found",
        409 => "conflict",
        422 => "invalid",
        other => panic!("no error code is named for status {other}"),
    }
}

fn expect_refusal(case: &str, step: &str, reply: &Reply, status: u16, words: &str) {
    expect_status(case, step, reply, status);
    assert_eq!(
        field(reply, "error"),
        code_of(status),
        "{case}: {step}: the error code: {}",
        text(reply)
    );
    let shown = text(reply);
    assert!(
        shown.contains(words),
        "{case}: {step}: the refusal should say {words:?}: {shown}"
    );
}

/// Waits until `id` has no operation in progress, and returns it.
async fn settled(subject: &dyn Subject, case: &str, id: &str) -> Reply {
    let path = format!("/api/workspaces/{id}");
    let deadline = tokio::time::Instant::now() + SETTLE;
    // flake-ok: this library code runs under the `testing` feature and the test kit's waits are dev-dependency only; the loop is bounded by SETTLE and names what it waited for.
    loop {
        let reply = subject.call("GET", &path, None).await;
        if reply.status == 200 && field(&reply, "busy").is_null() {
            return reply;
        }
        let shown = text(&reply);
        assert!(
            tokio::time::Instant::now() < deadline,
            "{case}: {id} never settled: {shown}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Creates the workspace of `case` and waits for it to settle; returns its id.
async fn created(subject: &dyn Subject, case: &str) -> String {
    let body = json!({
        "name": name(case),
        "repositories": [{"url": "https://example.org/acme/api.git"}],
    });
    let reply = subject.call("POST", "/api/workspaces", Some(body)).await;
    expect_status(case, "create", &reply, 202);
    let id = field(&reply, "id")
        .as_str()
        .unwrap_or_else(|| panic!("{case}: create returned no id: {}", text(&reply)))
        .to_owned();
    let idle = settled(subject, case, &id).await;
    // The lifecycle table ends a create in `running` (`phase::ended`), so both services do.
    assert_eq!(
        field(&idle, "status"),
        "running",
        "{case}: a create ends running: {}",
        text(&idle)
    );
    expect_status(
        case,
        "stop after create",
        &op(subject, &id, "stop", json!({})).await,
        202,
    );
    let idle = settled(subject, case, &id).await;
    assert_eq!(
        field(&idle, "status"),
        "stopped",
        "{case}: a case begins from a workspace that is down: {}",
        text(&idle)
    );
    id
}

async fn op(subject: &dyn Subject, id: &str, operation: &str, body: Value) -> Reply {
    subject
        .call(
            "POST",
            &format!("/api/workspaces/{id}/{operation}"),
            Some(body),
        )
        .await
}

async fn running(subject: &dyn Subject, case: &str) -> String {
    let id = created(subject, case).await;
    expect_status(
        case,
        "start",
        &op(subject, &id, "start", json!({})).await,
        202,
    );
    let up = settled(subject, case, &id).await;
    assert_eq!(field(&up, "status"), "running", "{case}: {}", text(&up));
    id
}

/// Every operation on an id nobody has is a 404.
async fn an_unknown_workspace_is_not_found(subject: &dyn Subject) {
    let case = "unknown";
    for operation in ["start", "stop", "reclaim", "reset-root-disk"] {
        let reply = op(subject, "ghost", operation, json!({})).await;
        expect_refusal(case, operation, &reply, 404, "ghost");
    }
    let check = subject
        .call("GET", "/api/workspaces/ghost/delete-check", None)
        .await;
    expect_refusal(case, "delete-check", &check, 404, "ghost");
    let grow = op(
        subject,
        "ghost",
        "grow-root-disk",
        json!({"size_mib": 99_999}),
    )
    .await;
    expect_refusal(case, "grow-root-disk", &grow, 404, "ghost");
    let gone = subject
        .call(
            "DELETE",
            "/api/workspaces/ghost",
            Some(json!({"confirm": true})),
        )
        .await;
    expect_refusal(case, "delete", &gone, 404, "ghost");
}

/// The workspace's `actions`, as the answer lists them.
fn actions(reply: &Reply) -> Vec<String> {
    field(reply, "actions")
        .as_array()
        .unwrap_or_else(|| panic!("no actions in {}", text(reply)))
        .iter()
        .map(|a| a.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// A stopped workspace refuses stop; a running one refuses start, a root-disk reset and a delete.
/// Each answer lists what its state admits, and nothing while an operation runs.
async fn the_state_decides_which_operation_is_admitted(subject: &dyn Subject) {
    let case = "states";
    let id = created(subject, case).await;
    let down = settled(subject, case, &id).await;
    let offered = actions(&down);
    assert!(
        ["start", "delete", "reset_root_disk"]
            .iter()
            .all(|a| offered.iter().any(|o| o == a))
            && !offered.iter().any(|o| o == "stop" || o == "attach"),
        "{case}: a workspace that is down offers start and delete, not stop: {offered:?}"
    );
    expect_refusal(
        case,
        "stop a stopped workspace",
        &op(subject, &id, "stop", json!({})).await,
        409,
        "is not running",
    );
    let starting = op(subject, &id, "start", json!({})).await;
    expect_status(case, "start", &starting, 202);
    assert_eq!(
        actions(&starting),
        Vec::<String>::new(),
        "{case}: nothing is offered while the start runs"
    );
    let up = settled(subject, case, &id).await;
    let offered = actions(&up);
    assert!(
        ["stop", "attach", "add_repository"]
            .iter()
            .all(|a| offered.iter().any(|o| o == a))
            && !offered
                .iter()
                .any(|o| o == "start" || o == "delete" || o == "reset_root_disk"),
        "{case}: a running workspace offers stop and attach, not start or delete: {offered:?}"
    );
    expect_refusal(
        case,
        "start a running workspace",
        &op(subject, &id, "start", json!({})).await,
        409,
        "is already running",
    );
    expect_refusal(
        case,
        "reset the root disk of a running workspace",
        &op(subject, &id, "reset-root-disk", json!({})).await,
        409,
        "stop it before resetting its root disk",
    );
    let delete = subject
        .call(
            "DELETE",
            &format!("/api/workspaces/{id}"),
            Some(json!({"confirm": true})),
        )
        .await;
    expect_refusal(
        case,
        "delete a running workspace",
        &delete,
        409,
        "stop it before deleting it",
    );
    expect_status(
        case,
        "stop",
        &op(subject, &id, "stop", json!({})).await,
        202,
    );
    let down = settled(subject, case, &id).await;
    assert_eq!(field(&down, "status"), "stopped", "{case}: {}", text(&down));
}

/// A root disk only grows: a size that is not above the current one is refused the same way.
async fn a_root_disk_only_grows(subject: &dyn Subject) {
    let case = "grow";
    let id = created(subject, case).await;
    let refused = op(subject, &id, "grow-root-disk", json!({"size_mib": 1})).await;
    expect_refusal(
        case,
        "grow below the current size",
        &refused,
        422,
        "a root disk only grows",
    );
}

/// Reclaim and grow wait out a start or a stop in progress, and succeed on a settled workspace.
async fn a_settled_workspace_reclaims_space(subject: &dyn Subject) {
    let case = "reclaim";
    let id = running(subject, case).await;
    expect_status(
        case,
        "reclaim",
        &op(subject, &id, "reclaim", json!({})).await,
        202,
    );
    settled(subject, case, &id).await;
}

/// Delete goes ahead on a clean workspace and refuses a fingerprint the check did not give.
async fn delete_checks_what_the_caller_confirmed(subject: &dyn Subject) {
    let case = "delete";
    let id = created(subject, case).await;
    let stale = subject
        .call(
            "DELETE",
            &format!("/api/workspaces/{id}"),
            Some(json!({"confirm": true, "fingerprint": "not-a-real-fingerprint"})),
        )
        .await;
    expect_refusal(
        case,
        "a stale fingerprint",
        &stale,
        409,
        "has changed since it was checked",
    );
    let deleted = subject
        .call(
            "DELETE",
            &format!("/api/workspaces/{id}"),
            Some(json!({"confirm": true})),
        )
        .await;
    expect_status(case, "delete", &deleted, 202);
    let path = format!("/api/workspaces/{id}");
    let deadline = tokio::time::Instant::now() + SETTLE;
    // flake-ok: this library code runs under the `testing` feature and the test kit's waits are dev-dependency only; the loop is bounded by SETTLE and names what it waited for.
    loop {
        let reply = subject.call("GET", &path, None).await;
        if reply.status == 404 {
            break;
        }
        let shown = text(&reply);
        assert!(
            tokio::time::Instant::now() < deadline,
            "{case}: {id} was never removed: {shown}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Two workspaces never share a name, at create and at rename.
async fn a_name_belongs_to_one_workspace(subject: &dyn Subject) {
    let case = "names";
    let first = created(subject, case).await;
    let body = json!({
        "name": name(case),
        "repositories": [{"url": "https://example.org/acme/api.git"}],
    });
    let again = subject.call("POST", "/api/workspaces", Some(body)).await;
    expect_status(case, "create under a taken name", &again, 409);
    let other = created(subject, "names-other").await;
    let rename = subject
        .call(
            "PATCH",
            &format!("/api/workspaces/{other}"),
            Some(json!({"name": name(case)})),
        )
        .await;
    expect_refusal(
        case,
        "rename to a taken name",
        &rename,
        409,
        "is already called",
    );
    let own = subject
        .call(
            "PATCH",
            &format!("/api/workspaces/{first}"),
            Some(json!({"name": name(case)})),
        )
        .await;
    expect_status(case, "rename to its own name", &own, 200);
}

/// A workspace that was stopped starts again, and each stop and start leaves it settled in the
/// status the API names.
async fn a_stopped_workspace_starts_again(subject: &dyn Subject) {
    let case = "restart";
    let id = running(subject, case).await;
    expect_status(
        case,
        "stop",
        &op(subject, &id, "stop", json!({})).await,
        202,
    );
    let down = settled(subject, case, &id).await;
    assert_eq!(field(&down, "status"), "stopped", "{case}: {}", text(&down));
    expect_status(
        case,
        "start again",
        &op(subject, &id, "start", json!({})).await,
        202,
    );
    let up = settled(subject, case, &id).await;
    assert_eq!(field(&up, "status"), "running", "{case}: {}", text(&up));
    assert_eq!(field(&up, "id"), id.as_str(), "{case}: {}", text(&up));
}

/// Runs every case against `subject`.
///
/// # Panics
///
/// With the case and step named, when the service under test breaks the contract.
pub async fn run(subject: &dyn Subject) {
    an_unknown_workspace_is_not_found(subject).await;
    the_state_decides_which_operation_is_admitted(subject).await;
    a_root_disk_only_grows(subject).await;
    a_settled_workspace_reclaims_space(subject).await;
    delete_checks_what_the_caller_confirmed(subject).await;
    a_name_belongs_to_one_workspace(subject).await;
    a_stopped_workspace_starts_again(subject).await;
}

#[cfg(test)]
mod tests;
