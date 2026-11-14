// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real Claude Code, GitHub CLI and Copilot CLI signing in and refreshing from inside a real
//! microVM, against the real services, with credentials made up for the test. No account is
//! involved: a made-up code, refresh token or access token is refused, and the refusal is what
//! these tools are watched through.
//!
//! The question is which hosts each tool calls for a login, a refresh and a first API call, because
//! login capture swaps a stand-in only toward the hosts of a profile (`puddle-logins`): a tool that
//! starts to use another host is a login the proxy would no longer see through. The host's own audit
//! says which hosts the guest connected to, so each flow is run in the guest and the audit read after
//! it. A flow fails when the host that must be called is missing or when a host outside the tool's
//! profile was asked for; it prints every host it saw either way.
//!
//! Tools and `nodejs` are installed in the guest from the live mirrors, and the tools talk to the
//! live services, so the test is named `vm_radar_*`: the per-pull-request jobs skip it and the
//! nightly `live-contracts` workflow runs it (an outage of a third party fails that run, never a
//! pull request). The tool versions are the ones `ci/login-tools/package.json` pins.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; what each tool called goes to stderr"
)]

mod support;
mod vm_support;

use std::collections::BTreeSet;
use std::time::Duration;

use puddle_e2e::lab::WebOptions;
use puddle_host::Host;
use puddle_logins::builtin;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_test_kit::eventually;
use puddle_types::{Host as HostName, WorkspaceId};
use serde_json::{Value, json};
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, guest_within, start_vm_host_platform, web_internet};

/// The versions CI runs the other login tests against; Dependabot bumps them.
const PINNED: &str = include_str!("../../../ci/login-tools/package.json");

/// Node 22 (Copilot CLI needs it, Debian 13 has 20): the release and the SHA-256 nodejs.org
/// publishes for each archive (`SHASUMS256.txt` of that release).
const NODE: &str = "v22.23.4";
const NODE_SHA256_X64: &str = "1805bcd35e6815559bd2eb97db438d3fdb0eb3a5e6a977b7c1badca76337e9ce";
const NODE_SHA256_ARM64: &str = "6287aa46b6c375d816e63ae66d024fbbfd70404825aabdfdf898a970871d2f4b";

/// The hosts the install reaches (live third parties) and the hosts of the login profiles. A
/// connection to any other host has no allow rule, so the audit records it and the tool does not get
/// through: that is what the test reports as a host outside the profile.
const INSTALL_HOSTS: &[&str] = &[
    "deb.debian.org",
    "security.debian.org",
    "nodejs.org",
    "registry.npmjs.org",
];
const PROFILE_HOSTS: &[&str] = &[
    "platform.claude.com",
    "console.anthropic.com",
    "api.anthropic.com",
    "claude.ai",
    "github.com",
    "api.github.com",
    "uploads.github.com",
    "api.individual.githubcopilot.com",
    "telemetry.individual.githubcopilot.com",
];

const FAKE_ACCESS: &str = "sk-ant-oat01-CANARY-fake-access-0000000000000000000000000000000000";
const FAKE_REFRESH: &str = "sk-ant-ort01-CANARY-fake-refresh-000000000000000000000000000000000";
const FAKE_GITHUB: &str = "gho_CANARY-fake-github-0000000000000000";

/// One run of a tool: what it is, the profile whose hosts it may call, the host it must call, and
/// the guest script (run as root, one `timeout` inside so a tool that waits for a person ends).
struct Flow {
    name: &'static str,
    profile: &'static str,
    must_call: &'static str,
    script: String,
}

fn flows() -> Vec<Flow> {
    let claude_home = "export HOME=/tmp/h-claude; mkdir -p $HOME";
    let github_home = "export HOME=/tmp/h-github GH_PROMPT_DISABLED=1 GH_BROWSER=true GH_NO_UPDATE_NOTIFIER=1; mkdir -p $HOME";
    vec![
        Flow {
            name: "claude auth login with a made-up code",
            profile: "claude",
            must_call: "platform.claude.com",
            script: format!(
                "{claude_home}; printf 'fakecode#fakestate\\n' | timeout 90 claude auth login; echo rc=$?"
            ),
        },
        Flow {
            name: "claude refreshing an expired made-up login",
            profile: "claude",
            must_call: "platform.claude.com",
            script: format!(
                "{claude_home}/.claude; cat > $HOME/.claude/.credentials.json <<'EOF'\n{}\nEOF\n\
                 timeout 90 claude -p 'say hi' --output-format json </dev/null; echo rc=$?",
                json!({"claudeAiOauth": {
                    "accessToken": FAKE_ACCESS, "refreshToken": FAKE_REFRESH,
                    "expiresAt": 1000, "scopes": ["user:inference", "user:profile"],
                    "subscriptionType": "pro"
                }})
            ),
        },
        Flow {
            name: "gh auth login --web (asks for a device code, nobody enters it)",
            profile: "github",
            must_call: "github.com",
            script: format!(
                "{github_home}; timeout 25 gh auth login --hostname github.com --git-protocol https --web </dev/null; echo rc=$?"
            ),
        },
        Flow {
            name: "gh api with a made-up token",
            profile: "github",
            must_call: "api.github.com",
            script: format!(
                "{github_home}; GH_TOKEN={FAKE_GITHUB} timeout 30 gh api user </dev/null; echo rc=$?"
            ),
        },
        Flow {
            // `--no-auto-update`: without it the tool also downloads its own update from
            // `release-assets.githubusercontent.com` (seen 2026-11-08, copilot 1.0.105), which is no
            // login traffic and carries no credential, so it is kept out of the question asked here.
            name: "copilot with a made-up token",
            profile: "github",
            must_call: "api.github.com",
            script: format!(
                "{github_home}; COPILOT_GITHUB_TOKEN={FAKE_GITHUB} timeout 90 copilot --no-auto-update -p 'say hi' </dev/null; echo rc=$?"
            ),
        },
    ]
}

/// The tool versions of `ci/login-tools/package.json`, as `name@version` for `npm install`.
fn pinned_packages() -> String {
    let json: Value = serde_json::from_str(PINNED).unwrap();
    let deps = json.get("dependencies").and_then(Value::as_object).unwrap();
    deps.iter()
        .map(|(name, version)| format!("{name}@{}", version.as_str().unwrap()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn install_script() -> String {
    format!(
        r#"
export DEBIAN_FRONTEND=noninteractive
{{ apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates curl xz-utils gh; }} >/tmp/apt.log 2>&1
echo "apt=$?"
case "$(uname -m)" in
  x86_64) arch=x64; sha={NODE_SHA256_X64} ;;
  aarch64) arch=arm64; sha={NODE_SHA256_ARM64} ;;
esac
f=node-{NODE}-linux-$arch.tar.xz
curl -fsSL https://nodejs.org/dist/{NODE}/$f -o /tmp/$f \
  && echo "$sha  /tmp/$f" | sha256sum -c - \
  && tar -xJf /tmp/$f -C /usr/local --strip-components=1
echo "node=$?"
npm install -g --no-audit --no-fund {} >/tmp/npm.log 2>&1
echo "npm=$?"
tail -3 /tmp/apt.log /tmp/npm.log
node --version; gh --version | head -1; claude --version; copilot --version | head -1
"#,
        pinned_packages()
    )
}

/// The connection records of the audit so far, in order.
fn connections(host: &Host<puddle_compute_msb::MsbRuntime>) -> Vec<Value> {
    host.store()
        .audit_lines(0, 200_000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, line)| serde_json::from_str::<Value>(&line).ok())
        .filter(|record| record["type"] == "connection")
        .collect()
}

/// The hosts of the connection records after the first `before`, with what the proxy decided.
fn hosts_since(vm: &VmHost, before: usize) -> BTreeSet<(String, String)> {
    connections(&vm.host)
        .iter()
        .skip(before)
        .map(|r| {
            (
                r["host"].as_str().unwrap_or("?").to_owned(),
                r["decision"].as_str().unwrap_or("?").to_owned(),
            )
        })
        .collect()
}

/// How long the audit must stay unchanged before a flow counts as over.
const QUIET: Duration = Duration::from_secs(5);

/// Waits until the audit has written no connection record for `QUIET` and returns how many there are.
///
/// A record is written when its connection ends, and a tool can leave connections behind it: the
/// proxy sees them close a moment after the tool exited (2026-11-08: a claude refresh's last
/// `api.anthropic.com` record landed in the window of the `gh` flow that followed). Starting the
/// next flow only after the audit is quiet keeps each flow's records in its own window.
async fn settled(vm: &VmHost) -> usize {
    let mut count = connections(&vm.host).len();
    let mut quiet_since = tokio::time::Instant::now();
    // flake-ok: waits for the absence of new records over a fixed quiet period, which `eventually` (a probe that turns true) cannot express.
    while quiet_since.elapsed() < QUIET {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = connections(&vm.host).len();
        if now != count {
            count = now;
            quiet_since = tokio::time::Instant::now();
        }
    }
    count
}

/// Whether the profile named `profile` has `host` among the hosts it swaps stand-ins toward.
fn in_profile(profile: &str, host: &str) -> bool {
    let profile = builtin().iter().find(|p| p.id == profile).unwrap();
    HostName::parse_normalised(host).is_ok_and(|host| profile.hosts().contains(&host))
}

fn last_line(out: &str) -> &str {
    out.lines().last().unwrap_or("")
}

/// The last few lines of a tool's output, joined, for the log.
fn tail(out: &str) -> String {
    let total = out.lines().count();
    out.lines()
        .skip(total.saturating_sub(8))
        .collect::<Vec<_>>()
        .join(" | ")
}

/// The hosts of a flow's connections that its profile does not cover.
fn outside_profile(profile: &str, seen: &BTreeSet<(String, String)>) -> Vec<String> {
    seen.iter()
        .filter(|(host, _)| !in_profile(profile, host))
        .map(|(host, decision)| format!("{host} ({decision})"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_login_tools_call_only_the_hosts_of_their_login_profile() {
    // No fake names: every name goes to the real internet through the company proxy.
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("logintools", net.options, &net.platform, |_| {}).await;
    let name = format!("{}-tools", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    for site in INSTALL_HOSTS.iter().chain(PROFILE_HOSTS) {
        vm.host
            .store()
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&NewRule {
                scope: Scope::Workspace(workspace.clone()),
                pattern: Pattern::parse(site).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 3072}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");

    let installed = guest_within(
        &vm.runtime,
        &workspace,
        &install_script(),
        Duration::from_mins(25),
    )
    .await;
    eprintln!("{installed}");
    for step in ["apt=0", "node=0", "npm=0"] {
        assert!(
            installed.contains(step),
            "the tools could not be installed ({step} missing; live third parties: the Debian mirrors, nodejs.org, the npm registry): {installed}"
        );
    }

    let mut failures = Vec::new();
    for flow in flows() {
        let before = settled(&vm).await;
        let out = guest_within(
            &vm.runtime,
            &workspace,
            &flow.script,
            Duration::from_mins(3),
        )
        .await;
        // A connection's record is written when it ends, after the tool exited: wait for the one
        // the flow must have made.
        eventually(
            &format!("{}: a connection record for {}", flow.name, flow.must_call),
            Duration::from_secs(60),
            || {
                let seen = hosts_since(&vm, before);
                if seen.iter().any(|(host, _)| host == flow.must_call) {
                    Ok(seen)
                } else {
                    // The failure message then names every host the flow did call.
                    Err(format!(
                        "hosts called: {seen:?}; the tool said: {}",
                        last_line(&out)
                    ))
                }
            },
        )
        .await;
        // Late records of this flow's own connections belong to it; the next flow starts after them.
        settled(&vm).await;
        let seen = hosts_since(&vm, before);
        let outside = outside_profile(flow.profile, &seen);
        eprintln!(
            "{}: {} host(s) called: {seen:?}\n  output: {}",
            flow.name,
            seen.len(),
            tail(&out)
        );
        if !outside.is_empty() {
            failures.push(format!(
                "{}: called {outside:?}, which the {} login profile does not cover",
                flow.name, flow.profile
            ));
        }
    }
    assert!(failures.is_empty(), "login hosts moved: {failures:#?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
