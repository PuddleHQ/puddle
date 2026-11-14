// SPDX-License-Identifier: AGPL-3.0-or-later
//! The environment of the managed tools layer in a real microVM (docs/plan/mise-build-plan.md,
//! M-4): the shims come first on `PATH` for every way into the guest (a login shell, which is what
//! a terminal and an SSH session run, a plain exec and `sudo -E`), the root `mise.toml` is seen
//! from a checkout below it, a repository's `[env]` needs the `repo_code` switch, and a workspace
//! that existed before the layer picks the variables up at its next start with its root disk kept.
//!
//! The binary is downloaded by hand into the folder the layer uses and `node` comes from the real
//! node.js site, so the test is named `vm_radar_*` (live third parties: mise's site, GitHub,
//! nodejs.org); the per-pull-request jobs skip it and the weekly `full-test` workflow runs it.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; what each step said goes to the log"
)]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime as _, Sandbox as _};
use puddle_e2e::lab::WebOptions;
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host_platform, web_internet};

/// The pinned release (M-3) and the checksum of its musl tarball.
const MISE_VERSION: &str = "v2026.10.21";
const MISE_SHA256: &str = "63b5621467b5b9950b99b7333aec1b4a34c5f23a9e464d330357c5b35502e8d7";

/// The hosts mise needs to install node.
const HOSTS: &[&str] = &[
    "mise.jdx.dev",
    "mise-versions.jdx.dev",
    "github.com",
    "release-assets.githubusercontent.com",
    "nodejs.org",
    "tuf-repo-cdn.sigstore.dev",
];

const LIVE_NOTE: &str =
    "live third parties: mise's site, GitHub, nodejs.org; an outage there fails this test";

/// A script as the session user in a login shell (a terminal or an SSH session), with its code
/// and output in the log.
#[expect(
    clippy::print_stderr,
    reason = "the test log: what each guest step said, read when a step fails"
)]
async fn terminal(vm: &VmHost, ws: &WorkspaceId, name: &str, script: &str) -> (i32, String) {
    let (code, out) = guest_as(&vm.runtime, ws, "agent", script, Duration::from_mins(20)).await;
    eprintln!("== {name} (exit {code})\n{out}");
    (code, out)
}

/// A program run the way an editor or an agent does: no shell, the sandbox's own environment.
async fn exec_as_agent(
    vm: &VmHost,
    ws: &WorkspaceId,
    program: &str,
    args: &[&str],
) -> (i32, String) {
    let handle = vm.runtime.get(&ws.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            ExecRequest::new(program, args.iter().copied())
                .as_user("agent")
                .with_timeout(Duration::from_mins(2)),
        )
        .await
        .unwrap();
    (
        out.status.code,
        format!("{}{}", out.stdout_text(), out.stderr_text()),
    )
}

async fn put(api: &Api, path: &str, body: serde_json::Value) {
    let reply = api
        .request("PUT", path, Some(&body.to_string()), true)
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

async fn restart(api: &Api, events: &mut support::Events, name: &'static str) {
    api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    events.until(ended(name), Duration::from_secs(120)).await;
    api.post(&format!("/api/workspaces/{name}/start"), "").await;
    let end = events.until(ended(name), Duration::from_secs(600)).await;
    assert_eq!(
        end.get("step").and_then(|s| s.as_str()),
        Some("done"),
        "{end}"
    );
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_mise_environment_reaches_every_way_into_the_guest_and_follows_the_trust_switch() {
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("miseenv", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    // The workspace exists before the layer is on: the global switch is off while it is made.
    put(&api, "/api/settings", json!({"tools": {"mise": false}})).await;
    let ws = create_workspace_from(&vm, &api, "miseenv", HOSTS, json!({})).await;
    let name: &'static str = Box::leak(ws.as_str().to_owned().into_boxed_str());
    let mount = format!("/workspaces/{ws}");
    let mut events = api.events().await;

    let (code, out) = terminal(
        &vm,
        &ws,
        "off",
        "echo \"mise_vars=$(env | grep -c '^MISE_')\"; echo marker > $HOME/marker",
    )
    .await;
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("mise_vars=0"), "with the layer off: {out}");

    // Switched on, the next start gives the variables and keeps the root disk (the marker).
    put(&api, "/api/settings", json!({"tools": {"mise": true}})).await;
    restart(&api, &mut events, name).await;
    let (code, out) = terminal(
        &vm,
        &ws,
        "on",
        "echo \"data=$MISE_DATA_DIR\"; echo \"ceiling=$MISE_CEILING_PATHS\"; \
         echo \"trusted=$MISE_TRUSTED_CONFIG_PATHS\"; echo \"marker=$(cat $HOME/marker)\"; \
         echo \"path=$PATH\"",
    )
    .await;
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(&format!("data={mount}/.puddle/mise/data\n")),
        "{out}"
    );
    assert!(out.contains("ceiling=/workspaces\n"), "{out}");
    assert!(out.contains(&format!("trusted={mount}\n")), "{out}");
    assert!(
        out.contains("marker=marker"),
        "the root disk was kept: {out}"
    );
    assert!(
        out.contains(&format!("{mount}/.puddle/mise/data/shims:/usr/local/sbin")),
        "shims ahead of the system folders, even after /etc/profile reset PATH (msb adds its own folder before): {out}"
    );

    // The pinned binary in the layer's folder (the fetch of M-3 puts it there once it exists).
    let (code, out) = terminal(
        &vm,
        &ws,
        "binary",
        &format!(
            "set -e; b={mount}/.puddle/mise/bin; mkdir -p $b; \
             if [ ! -x $b/mise ]; then t=$(mktemp -d) && cd $t \
             && curl -fsSL -o mise.tgz https://mise.jdx.dev/{MISE_VERSION}/mise-{MISE_VERSION}-linux-x64-musl.tar.gz \
             && echo '{MISE_SHA256}  mise.tgz' | sha256sum -c - && tar xzf mise.tgz \
             && mv -n mise/bin/mise $b/mise; fi; mise --version"
        ),
    )
    .await;
    assert_eq!(code, 0, "{out}\n({LIVE_NOTE})");

    // A root mise.toml with node, a checkout below it with its own file that has `[env]`.
    let (code, out) = terminal(
        &vm,
        &ws,
        "install",
        &format!(
            "set -e; cd {mount}; printf '[tools]\\nnode = \"22\"\\n' > mise.toml; \
             mkdir -p repo; printf '[env]\\nPUDDLE_REPO_ENV = \"yes\"\\n' > repo/mise.toml; \
             mise install --yes; mise reshim; \
             cd repo && echo \"configs=$(mise config ls 2>&1 | wc -l)\"; \
             echo \"node=$(node --version)\""
        ),
    )
    .await;
    assert_eq!(code, 0, "{out}\n({LIVE_NOTE})");
    assert!(
        out.contains("configs=2"),
        "the root file is seen from the checkout: {out}"
    );
    assert!(out.contains("node=v22."), "{out}");

    // Every way in finds node: terminal and SSH sessions (login shells), a plain exec, sudo -E.
    let (code, out) = terminal(
        &vm,
        &ws,
        "terminal",
        &format!("cd {mount}/repo && node --version"),
    )
    .await;
    assert!(code == 0 && out.contains("v22."), "{out}");
    let (code, out) = exec_as_agent(&vm, &ws, "node", &["--version"]).await;
    assert!(code == 0 && out.contains("v22."), "a plain exec: {out}");
    let (code, out) = terminal(
        &vm,
        &ws,
        "sudo",
        "sudo -E node --version && sudo node --version",
    )
    .await;
    assert!(code == 0 && out.matches("v22.").count() == 2, "sudo: {out}");

    // The repository's [env] runs only with `repo_code` on, which it is by default.
    let (code, out) = terminal(
        &vm,
        &ws,
        "trusted",
        &format!("cd {mount}/repo && node -p process.env.PUDDLE_REPO_ENV"),
    )
    .await;
    assert!(code == 0 && out.contains("yes"), "with repo_code on: {out}");
    put(
        &api,
        &format!("/api/settings/workspaces/{name}"),
        json!({"tools": {"mise": true, "repo_code": false}}),
    )
    .await;
    restart(&api, &mut events, name).await;
    let (code, out) = terminal(
        &vm,
        &ws,
        "untrusted",
        &format!("cd {mount}/repo && node --version"),
    )
    .await;
    assert_ne!(code, 0, "the shim runs the untrusted [env]: {out}");
    assert!(out.contains("not trusted"), "{out}");
    // Outside the repository the root file (tools only) still works.
    let (code, out) = terminal(
        &vm,
        &ws,
        "root file",
        &format!("cd {mount} && node --version"),
    )
    .await;
    assert!(code == 0 && out.contains("v22."), "{out}");
    put(
        &api,
        &format!("/api/settings/workspaces/{name}"),
        json!({"tools": {"mise": true, "repo_code": true}}),
    )
    .await;
    restart(&api, &mut events, name).await;
    let (code, out) = terminal(
        &vm,
        &ws,
        "trusted again",
        &format!("cd {mount}/repo && node --version"),
    )
    .await;
    assert!(code == 0 && out.contains("v22."), "{out}");
}
