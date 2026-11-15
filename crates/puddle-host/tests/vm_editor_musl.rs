// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor on an Alpine image (musl, no glibc) on a real microVM (K on Linux KVM): the
//! workspace starts and runs commands, the editor server does not run, and the user is told why in
//! a problem on the workspace instead of finding a window that never loads.
#![cfg(target_os = "linux")]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, guest, start_vm_host_config};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_editor_on_an_alpine_image_says_why_it_is_unavailable_and_the_workspace_still_works() {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("musl", named_id_options(), |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-musl", settings.prefix.as_str());
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

    // The image is musl, as the test assumes.
    guest(
        &runtime,
        &sandbox,
        "ls /lib/ld-musl-* && ! ls /lib/ld-linux-* /lib64/ld-linux-* 2>/dev/null",
    )
    .await;

    let key = format!("editor-install:{name}");
    let problem = puddle_test_kit::eventually_async(
        "the browser editor's musl problem",
        Duration::from_secs(60),
        || async { host.problems().list().into_iter().find(|p| p.key == key) },
    )
    .await;
    assert_eq!(
        problem.title,
        format!("The browser editor is not available in {name}: its image uses musl")
    );
    assert!(
        problem.detail.contains("desktop VS Code over direct SSH"),
        "{}",
        problem.detail
    );

    // No server runs and none got a password; the workspace itself is fine.
    guest(
        &runtime,
        &sandbox,
        "! test -e /run/puddle/editor-password && ! nc -z 127.0.0.1 41337 2>/dev/null",
    )
    .await;
    assert_eq!(
        guest(&runtime, &sandbox, "echo alive").await.trim(),
        "alive"
    );

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
