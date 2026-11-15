// SPDX-License-Identifier: AGPL-3.0-or-later
//! An image with no `sudo` on a real microVM (K on Linux KVM): the workspace starts as the session
//! user `agent`, `agent` cannot become root, and the user is told so in a problem on the workspace,
//! with the way out. Switching the session user to root and starting again ends the problem.
#![cfg(target_os = "linux")]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended};
use vm_support::{VmHost, guest, start_vm_host};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_an_image_with_no_sudo_is_a_problem_and_a_switch_to_root_clears_it() {
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host("nosudo").await;
    let name = format!("{}-nosudo", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "image": "alpine:3", "memory_mib": 1024})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");

    // The image is a stock Alpine: the session user exists and there is no sudo anywhere.
    guest(
        &runtime,
        &sandbox,
        "id agent && ! command -v sudo && ! ls /usr/bin/sudo /usr/local/bin/sudo 2>/dev/null",
    )
    .await;

    let key = format!("no-sudo:{name}");
    let problem = puddle_test_kit::eventually_async(
        "the no-sudo problem",
        Duration::from_secs(60),
        || async { host.problems().list().into_iter().find(|p| p.key == key) },
    )
    .await;
    assert_eq!(
        problem.title,
        format!("{name} has no sudo, so its user cannot become root")
    );
    assert!(
        problem.detail.contains("use an image that has `sudo`")
            && problem.detail.contains("session user to root"),
        "{}",
        problem.detail
    );

    // The workspace itself works as `agent`.
    assert_eq!(
        guest(&runtime, &sandbox, "echo alive").await.trim(),
        "alive"
    );

    // Switched to root, the next start boots with no session user that needs sudo.
    let put = api
        .put(
            &format!("/api/workspaces/{name}/session-user"),
            &json!({"session_user": "root"}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    for step in ["stop", "start"] {
        let reply = api
            .post(&format!("/api/workspaces/{name}/{step}"), "")
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(600))
            .await;
        assert_eq!(end["step"], "done", "{end}");
    }
    assert!(
        host.problems().list().iter().all(|p| p.key != key),
        "{:?}",
        host.problems().list()
    );

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
