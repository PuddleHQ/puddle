// SPDX-License-Identifier: AGPL-3.0-or-later
//! The non-root session user on a real microVM (K on Linux KVM, W on Windows WHP), with the real
//! tools at their widest: a new workspace's user is `agent`, becomes root with `sudo`, installs
//! system packages through `sudo`, runs Claude Code with `--dangerously-skip-permissions` (which
//! Claude Code refuses as root), installs Python packages into its own home, clones and commits
//! in a checkout, and serves on port 80. Then the workspace is switched to root and its next start
//! gives the checkout back to root. Nothing may fail because of the sandbox.
//!
//! The tools reach the live registries on purpose (Debian, npm, the Python Package Index, GitHub,
//! Anthropic's API):
//! the bar is the real tools, and the failure messages name the third party.
//! So it is part of the real-tool radar (the name starts `vm_radar_`: weekly, or with the
//! `full-test` label), not the per-pull-request gate, which covers the session user with
//! `vm_session_user_*` in puddle-boot and the host tests that clone and log in as `agent`.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: a failed step fails the test"
)]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended};
use vm_support::{VmHost, guest, start_vm_host};

/// What the tools reach, all live.
const LIVE: &[&str] = &[
    "deb.debian.org",
    "security.debian.org",
    "registry.npmjs.org",
    "pypi.org",
    "files.pythonhosted.org",
    "github.com",
    "codeload.github.com",
    "api.anthropic.com",
    "statsig.anthropic.com",
    "storage.googleapis.com",
    "downloads.claude.ai",
];

const LIVE_NOTE: &str =
    "live third parties: Debian, npm, PyPI, GitHub, Anthropic; an outage there fails this test";

/// Claude Code's refusal of `--dangerously-skip-permissions` for root.
const ROOT_REFUSAL: &str = "cannot be used with root/sudo privileges";

/// Runs `script` in bash as `user`, the way a terminal of the workspace does (a login shell), and
/// returns its exit code and its output.
async fn run_as(vm: &VmHost, sandbox: &WorkspaceId, user: &str, script: &str) -> (i32, String) {
    let handle = vm.runtime.get(&sandbox.sandbox_name()).await.unwrap();
    let out = handle
        .exec(
            // The script is an argument, not stdin, so a tool that reads stdin cannot eat the
            // rest of it.
            ExecRequest::sh(format!(
                "cd && exec bash -l -c '{}'",
                script.replace('\'', "'\\''")
            ))
            .as_user(user)
            .with_timeout(Duration::from_mins(15)),
        )
        .await
        .unwrap_or_else(|e| panic!("the guest script did not finish: {e} ({LIVE_NOTE})\n{script}"));
    (
        out.status.code,
        format!("{}{}", out.stdout_text(), out.stderr_text()),
    )
}

/// [`run_as`] that must succeed.
async fn ok_as(vm: &VmHost, sandbox: &WorkspaceId, user: &str, script: &str) -> String {
    let (code, said) = run_as(vm, sandbox, user, script).await;
    assert_eq!(code, 0, "as {user}: {script}\n{said}\n({LIVE_NOTE})");
    said
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_session_user_agent_runs_the_real_tools_at_their_widest_and_a_switch_to_root_takes_back_the_files()
 {
    let vm = start_vm_host("sesu").await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let name = format!("{}-sesu", vm.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    for site in LIVE {
        puddle_store::testing::own(vm.host.store(), &sandbox);
        vm.host
            .store()
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&NewRule {
                scope: Scope::Workspace(sandbox.clone()),
                pattern: Pattern::parse(site).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 4096}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    let ws = api.get(&format!("/api/workspaces/{name}")).await.json();
    assert_eq!(
        ws["session_user"], "agent",
        "a new workspace is agent's: {ws}"
    );
    let volume = format!("/workspaces/{name}");

    check_identity_and_claude_code(&vm, &sandbox).await;
    check_python_git_and_low_ports(&vm, &sandbox, &volume).await;

    // Switched to root: the next start boots as root and gives the volume back to root.
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
    let owners = guest(
        &vm.runtime,
        &sandbox,
        &format!("stat -c '%U' {volume} {volume}/hw {volume}/hw/README; git -C {volume}/hw log -1 --format=%s"),
    )
    .await;
    assert_eq!(owners.trim(), "root\nroot\nroot\nfrom-agent", "{owners}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Who the session is, root at will, system packages and npm's global install through `sudo`,
/// and Claude Code at its widest.
async fn check_identity_and_claude_code(vm: &VmHost, sandbox: &WorkspaceId) {
    let who = ok_as(
        vm,
        sandbox,
        "agent",
        "id -un; echo \"$HOME\"; sudo -n true && echo sudo-ok; sudo -n -i sh -c 'id -un; pwd'; \
         stat -c '%U' ~/.vscode-server/data/Machine/settings.json",
    )
    .await;
    assert_eq!(
        who.trim(),
        "agent\n/home/agent\nsudo-ok\nroot\n/root\nagent",
        "{who}"
    );

    // System packages through sudo, from Debian, then npm's global install through sudo.
    ok_as(
        vm,
        sandbox,
        "agent",
        "export DEBIAN_FRONTEND=noninteractive
         sudo -n -E apt-get update -qq
         sudo -n -E apt-get install -y -qq nodejs npm python3 python3-pip python3-venv git curl >/tmp/apt.log 2>&1 \
           || { tail -20 /tmp/apt.log; exit 1; }
         sudo -n npm install -g --no-audit --no-fund @anthropic-ai/claude-code
         claude --version",
    )
    .await;

    // Claude Code at its widest: as agent the flag is taken and the call reaches Anthropic's API
    // (a made-up key, so the answer is an authentication error); as root the same call is refused
    // before it starts. Its settings land in agent's home, agent's own.
    let (_, as_agent) = run_as(
        vm,
        sandbox,
        "agent",
        "ANTHROPIC_API_KEY=sk-ant-puddle-not-a-key timeout 120 \
           claude --dangerously-skip-permissions -p 'say hi' </dev/null 2>&1; echo \"exit=$?\"
         stat -c 'config %U' ~/.claude.json ~/.claude 2>&1",
    )
    .await;
    assert!(!as_agent.contains(ROOT_REFUSAL), "{as_agent}");
    assert!(
        !as_agent.contains("exit=124"),
        "Claude Code hung: {as_agent}"
    );
    assert!(
        as_agent.to_lowercase().contains("api key"),
        "Claude Code did not get as far as Anthropic's answer to the key ({LIVE_NOTE}): {as_agent}"
    );
    assert!(
        as_agent.contains("config agent") && !as_agent.contains("config root"),
        "{as_agent}"
    );
    let (_, as_root) = run_as(
        vm,
        sandbox,
        "agent",
        "sudo -n claude --dangerously-skip-permissions -p 'say hi' </dev/null 2>&1; echo \"exit=$?\"",
    )
    .await;
    assert!(as_root.contains(ROOT_REFUSAL), "{as_root}");
}

/// Python packages in agent's home and a virtual environment, a clone and a commit in the volume,
/// and a port below 1024, all as agent.
async fn check_python_git_and_low_ports(vm: &VmHost, sandbox: &WorkspaceId, volume: &str) {
    // Python: pip into agent's own home, and a virtual environment.
    let python = ok_as(
        vm,
        sandbox,
        "agent",
        "python3 -m pip install --user --break-system-packages --no-warn-script-location -q six
         python3 -c 'import six, sys; print(\"user-site\", six.__file__.startswith(sys.argv[1]))' \"$HOME/.local\"
         python3 -m venv ~/venv && ~/venv/bin/pip install -q six && ~/venv/bin/python -c 'import six; print(\"venv-ok\")'",
    )
    .await;
    assert!(
        python.contains("user-site True") && python.contains("venv-ok"),
        "{python}"
    );

    // git: a clone into the volume and a commit, as agent; root reads it too.
    let git = ok_as(
        vm,
        sandbox,
        "agent",
        &format!(
            "cd {volume} && git clone -q https://github.com/octocat/Hello-World.git hw && cd hw
             git -c user.name=Agent -c user.email=agent@example.invalid commit -q --allow-empty -m from-agent
             git log -1 --format='%an %s'; git status --porcelain | wc -l
             sudo -n git -C {volume}/hw log -1 --format='root sees %s'"
        ),
    )
    .await;
    assert!(
        git.contains("Agent from-agent\n0\n") && git.contains("root sees from-agent"),
        "{git}"
    );

    // A port below 1024, without root.
    let port = ok_as(
        vm,
        sandbox,
        "agent",
        "python3 -m http.server 80 --bind 127.0.0.1 >/tmp/http.log 2>&1 & pid=$!
         for _ in $(seq 50); do curl -s -o /dev/null http://127.0.0.1:80/ && break; sleep 0.2; done
         curl -s -o /dev/null -w 'port80=%{http_code}\\n' http://127.0.0.1:80/; kill $pid",
    )
    .await;
    assert!(port.contains("port80=200"), "{port}");
}
