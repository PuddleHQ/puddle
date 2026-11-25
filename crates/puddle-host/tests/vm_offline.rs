// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace whose image is cached starts while the route to the registry is dead (QA playbook
//! 07, S17b), on a real microVM (K on Linux KVM). The host's way out to the registry is the pull
//! proxy and the company-proxy route behind it; here that route is switched to a proxy nobody
//! listens on after the workspace was created, so any request to a registry fails.
//!
//! The control first: with the route dead, pulling an image that is not in the cache fails with
//! the registry named, so the later start is not a start that happened to have a route.

mod support;
mod vm_support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use puddle_compute::{ComputeError, Runtime};
use puddle_types::{ImageRef, WorkspaceId};
use puddle_upstream::{Discovery, FakeOs, Hop, ProxyAddr, ProxyConfig};
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{guest, start_vm_host_with};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_a_workspace_with_a_cached_image_starts_while_the_pull_route_is_dead() {
    // A proxy address nothing listens on: the port stays ours, so connecting is refused.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket
        .bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .unwrap();
    let dead_port = socket.local_addr().unwrap().port();
    let route_is_dead = Arc::new(AtomicBool::new(false));
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/proxy.pac".into()),
        ..ProxyConfig::default()
    });
    {
        let dead = Arc::clone(&route_is_dead);
        os.set_pac(move |_| {
            Ok(if dead.load(Ordering::SeqCst) {
                vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", dead_port))]
            } else {
                vec![Hop::Direct]
            })
        });
    }
    let mut options = named_id_options();
    options.discovery = Some(Discovery::new(
        Arc::clone(&os) as _,
        puddle_upstream::Config::default(),
    ));
    let vm = start_vm_host_with("cached", options).await;
    let name = format!("{}-cached", vm.settings.prefix.as_str());
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let id = WorkspaceId::new(&name).unwrap();
    puddle_store::testing::own(vm.host.store(), &id);
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;

    // Created with a working route: the image is pulled (or already cached) and the workspace runs.
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 1024}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    let stop = api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(120))
        .await;
    assert_eq!(end["step"], "done", "{end}");

    // The route dies.
    route_is_dead.store(true, Ordering::SeqCst);
    os.fire_change();
    let other = ImageRef::new("registry.lab.invalid/not-cached:1").unwrap();
    let err = vm.runtime.pull_image(&other).await.unwrap_err();
    let ComputeError::ImagePull { reason, .. } = &err else {
        panic!("the control pull did not fail as a pull: {err:?}");
    };
    assert!(
        reason.contains("could not connect") && reason.contains("registry.lab.invalid"),
        "the control pull's reason: {reason}"
    );

    // The cached image needs no route: the start goes through.
    let reply = api.post(&format!("/api/workspaces/{name}/start"), "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(300))
        .await;
    assert_eq!(end["step"], "done", "the start needed the registry: {end}");
    assert_eq!(
        api.get(&format!("/api/workspaces/{name}")).await.json()["status"],
        "running"
    );
    let out = guest(&vm.runtime, &id, "echo alive").await;
    assert!(out.contains("alive"), "{out}");
    vm.host.shutdown().await;
    drop(socket);
}
