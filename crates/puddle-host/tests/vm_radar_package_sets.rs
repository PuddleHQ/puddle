// SPDX-License-Identifier: AGPL-3.0-or-later
//! The package managers on the built-in rule sets, and the approve-and-retry loop, with the real
//! tools in a real microVM (K on Linux KVM, W on Windows WHP): what a person does on the first
//! day, with no rule of their own.
//!
//! Leg 1: a workspace whose only allowances are the sets "Package registries", "Debian and Ubuntu
//! packages" and "GitHub", switched on through the API. As the session user it runs `pnpm install
//! --frozen-lockfile` with install scripts, the same with `--prefer-offline` after `node_modules`
//! is gone, `npx playwright install chromium`, a `pip install -r` with a source build, `uv pip
//! install`, rustup, `cargo install cargo-nextest --locked`, a cargo dependency from an HTTPS git
//! URL and `docker run python:3.12-slim pip install requests` in the nested Docker. A step passes
//! with exit code 0 and no retry or error line in its log. After each tool the audit says which
//! hosts it reached: the ones the three sets decided are not listed anywhere, the ones they do not
//! cover (`OUTSIDE_THE_SETS`, each with the tool that needs it) are allowed by explicit workspace
//! rules made before the run and must equal the list, and the inbox must hold no open row. A new
//! host a tool starts to need, or a host a set stops covering, fails the run and names itself.
//!
//! Leg 2: no rule at all. Rustup, then `cargo build && cargo test` of a small crate. Every failure
//! prints the text a guest tool reads for each waiting host, the test approves the open rows through
//! `/api/pending` and the step runs again: green within [`MAX_ROUNDS`], and the rounds it took and
//! the hosts it approved are logged.
//!
//! Breakage: with `files.pythonhosted.org` switched off in "Package registries" the pip step fails
//! and the report names that host.
//!
//! The tools reach the live registries on purpose, so the test is named `vm_radar_*` (weekly, never
//! per pull request) and every failure message names the third parties.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test; what each step said goes to the log"
)]

mod support;
mod vm_support;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::Api;
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host};

/// The live third parties, for the failure messages.
const LIVE_NOTE: &str = "live third parties: Debian, npm, PyPI, crates.io, static.rust-lang.org, \
    GitHub, the Playwright CDN, Docker Hub; an outage there fails this test";

/// The sets of leg 1, by slug.
const SETS: [&str; 3] = ["package-registries", "debian-ubuntu", "github"];

/// The most rounds of approve-and-retry leg 2 may take before it fails: five approvals are the
/// playbook's limit before approval fatigue (docs/qa/01 S5), so six rounds with the green one.
const MAX_ROUNDS: usize = 6;

/// What the lines that show a tool retrying or failing look like. A retry is a failure the tool
/// hid. Same words as the radar of `vm_host.rs`.
const SUSPICIOUS: &str = "Err:|W: Failed|^E: |retrying|will retry|timed out|ETIMEDOUT|ECONNRESET|EPIPE|reset by peer|connection refused|EAI_AGAIN|unexpected eof|\\bstalled|handshake fail|handshake_failure|handshake (timeout|error)|handshakeexception|certificate verify|SSL_ERROR|SSL certificate problem|x509: |PKIX|spurious network error|i/o timeout|dial tcp|could not transfer|transfer failed|but failed|error sending request|Connection (refused|reset)|403 Forbidden|waiting for approval";

/// The hosts the three sets do not cover, with the tool that needs each: explicit, so a new one
/// fails the run. Each is allowed by a workspace rule made before the run.
const OUTSIDE_THE_SETS: &[(&str, &[&str])] = &[
    (
        "playwright",
        &[
            "cdn.playwright.dev",
            "playwright.download.prss.microsoft.com",
        ],
    ),
    ("rustup", &["sh.rustup.rs", "static.rust-lang.org"]),
    (
        "docker",
        &[
            "registry-1.docker.io",
            "auth.docker.io",
            "production.cloudfront.docker.com",
        ],
    ),
];

/// One tool of the radar: a name, a guest script run as the session user, and a time limit.
struct Tool {
    name: &'static str,
    script: &'static str,
    limit: Duration,
}

const APT_SETUP: &str = r#"
sudo apt-get update
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
  nodejs npm python3 python3-venv python3-dev python3-pip build-essential git curl ca-certificates docker.io docker-cli
. /etc/os-release; echo "$PRETTY_NAME"
command -v docker || dpkg -l 'docker*' | tail -5
docker --version
"#;

const NPM_PNPM: &str = r"
sudo npm install -g pnpm@9.15.0
pnpm --version
";

/// Written by [`PROJECT_FILES`]; the lockfile is made first, so the frozen install is real.
const PNPM_LOCK: &str = r"
cd ~/js
pnpm install --lockfile-only --reporter=append-only
test -s pnpm-lock.yaml
";

const PNPM_FROZEN: &str = r"
cd ~/js
CI=true pnpm install --frozen-lockfile --reporter=append-only
# The install scripts ran: esbuild's binary answers.
node_modules/.bin/esbuild --version
";

const PNPM_OFFLINE: &str = r"
cd ~/js
rm -rf node_modules
CI=true pnpm install --frozen-lockfile --prefer-offline --reporter=append-only
node_modules/.bin/esbuild --version
";

const PLAYWRIGHT: &str = r"
cd ~/js
npx playwright install chromium
ls ~/.cache/ms-playwright | grep -q chromium
";

const PIP: &str = r"
python3 -m venv ~/py
. ~/py/bin/activate
pip install --disable-pip-version-check -r ~/requirements.txt
# The source build left a working module.
python -c 'import yaml, numpy, pandas; print(yaml.__version__)'
";

const UV: &str = r"
. ~/py/bin/activate
pip install --disable-pip-version-check uv
uv venv ~/uvenv
uv pip install --python ~/uvenv -r ~/requirements.txt
~/uvenv/bin/python -c 'import yaml, numpy, pandas'
";

const RUSTUP: &str = r"
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. ~/.cargo/env
rustc --version
";

const NEXTEST: &str = r"
. ~/.cargo/env
cargo install cargo-nextest --locked
cargo nextest --version
";

const GIT_DEPENDENCY: &str = r"
. ~/.cargo/env
cd ~/gitdep
cargo fetch
";

const DOCKER: &str = r"
sudo mkdir -p /var/lib/docker
(sudo nohup dockerd >/tmp/dockerd.log 2>&1 &)
for i in $(seq 1 60); do sudo docker info >/dev/null 2>&1 && break; sleep 1; done
sudo docker info >/dev/null
sudo docker run --rm python:3.12-slim pip install --root-user-action=ignore requests
";

/// Writes the files the steps read.
const PROJECT_FILES: &str = r#"
mkdir -p ~/js ~/gitdep/src
cat > ~/js/package.json <<'JSON'
{"name":"radar-js","version":"1.0.0","private":true,"dependencies":{
 "vite":"5.4.9","esbuild":"0.24.0","typescript":"5.6.3","eslint":"8.57.1","@playwright/test":"1.55.0",
 "sharp":"0.33.5","next":"14.2.15","react":"18.3.1","react-dom":"18.3.1","lodash":"4.17.21"}}
JSON
cat > ~/requirements.txt <<'REQ'
--no-binary=pyyaml
numpy==2.1.2
pandas==2.2.3
scipy==1.14.1
scikit-learn==1.5.2
matplotlib==3.9.2
requests==2.32.3
django==5.1.2
httpx==0.27.2
pyyaml==6.0.2
REQ
cat > ~/gitdep/Cargo.toml <<'TOML'
[package]
name = "gitdep"
version = "0.1.0"
edition = "2021"
[dependencies]
semver_from_git = { package = "semver", git = "https://github.com/dtolnay/semver", tag = "1.0.23" }
TOML
echo 'fn main() {}' > ~/gitdep/src/main.rs
"#;

fn tools() -> Vec<Tool> {
    let t = |name, script, mins| Tool {
        name,
        script,
        limit: Duration::from_mins(mins),
    };
    vec![
        t("apt_setup", APT_SETUP, 15),
        t("npm_pnpm", NPM_PNPM, 5),
        t("pnpm_lock", PNPM_LOCK, 10),
        t("pnpm_frozen", PNPM_FROZEN, 15),
        t("pnpm_offline", PNPM_OFFLINE, 10),
        t("playwright", PLAYWRIGHT, 10),
        t("pip", PIP, 20),
        t("uv", UV, 15),
        t("rustup", RUSTUP, 10),
        t("nextest", NEXTEST, 25),
        t("git_dependency", GIT_DEPENDENCY, 5),
        t("docker", DOCKER, 10),
    ]
}

/// `body` as one logged step: its output goes to a file; the wrapper prints `RADAR step=` with the
/// exit code and the number of suspicious lines, then the lines themselves and the log's tail.
fn logged(name: &str, body: &str) -> String {
    format!(
        "cat > /tmp/step-{name}.sh <<'PUDDLE_STEP_EOF'\nset -e -o pipefail\n{body}\nPUDDLE_STEP_EOF\n\
         bash /tmp/step-{name}.sh >/tmp/step-{name}.log 2>&1 </dev/null\nrc=$?\n\
         bad=$(grep -c -i -E '{SUSPICIOUS}' /tmp/step-{name}.log)\n\
         echo \"RADAR step={name} rc=$rc suspicious_lines=$bad\"\n\
         if [ \"$rc\" != 0 ] || [ \"$bad\" != 0 ]; then\n\
           grep -i -E '{SUSPICIOUS}' /tmp/step-{name}.log | head -8 | sed 's/^/  | /'\n\
           tail -n 15 /tmp/step-{name}.log | sed 's/^/  > /'\n\
         fi"
    )
}

/// What one step of the radar did.
#[derive(Debug, Clone)]
struct Report {
    name: String,
    rc: String,
    suspicious: String,
    /// The guest's own words for a failing or retrying step.
    said: String,
    /// Hosts the three sets let through, hosts a rule let through, hosts the proxy refused.
    by_set: BTreeSet<String>,
    by_rule: BTreeSet<String>,
    refused: BTreeSet<String>,
    /// The open inbox rows after the step, `host:port`.
    open_rows: Vec<String>,
}

impl Report {
    /// Why the step is not a pass, or `None`: against the hosts `expected` (outside the sets) it may
    /// reach.
    fn failure(&self, expected: &BTreeSet<String>) -> Option<String> {
        let mut why = Vec::new();
        if self.rc != "0" {
            why.push(format!("exit code {}", self.rc));
        }
        if self.suspicious != "0" {
            why.push(format!("{} retry or error lines", self.suspicious));
        }
        if !self.refused.is_empty() {
            why.push(format!("hosts the proxy refused: {:?}", self.refused));
        }
        if !self.open_rows.is_empty() {
            why.push(format!(
                "hosts asked for and not allowed (open inbox rows): {:?}",
                self.open_rows
            ));
        }
        if &self.by_rule != expected {
            why.push(format!(
                "hosts outside the sets that it reached {:?}, expected {expected:?}",
                self.by_rule
            ));
        }
        (!why.is_empty()).then(|| {
            format!(
                "{}: {} ({LIVE_NOTE})\n{}",
                self.name,
                why.join("; "),
                self.said
            )
        })
    }
}

/// The field after `key` on the line of `out` that starts with `prefix`.
fn word<'a>(out: &'a str, prefix: &str, key: &str) -> &'a str {
    out.lines()
        .rev()
        .find(|l| l.starts_with(prefix))
        .and_then(|l| l.split_whitespace().find_map(|w| w.strip_prefix(key)))
        .unwrap_or("?")
}

/// The connection records of the workspace so far.
fn records_of(vm: &VmHost, workspace: &WorkspaceId) -> Vec<Value> {
    vm.host
        .store()
        .audit_lines(0, 500_000)
        .unwrap()
        .into_iter()
        .filter_map(|(_, line)| serde_json::from_str::<Value>(&line).ok())
        .filter(|r| r["type"] == "connection" && r["workspace_id"] == workspace.as_str())
        .collect()
}

/// The open inbox rows of the workspace, `host:port`.
fn open_rows(vm: &VmHost, workspace: &WorkspaceId) -> Vec<String> {
    vm.host
        .store()
        .open_pending_every_user(&puddle_store::testing::all_users(), Some(workspace))
        .unwrap()
        .iter()
        .map(|r| format!("{}:{}", r.host, r.port))
        .collect()
}

/// Runs one tool and reports it, with the hosts it reached since the last report.
async fn run_tool(
    vm: &VmHost,
    workspace: &WorkspaceId,
    tool: &Tool,
    seen: &mut usize,
    reported: &mut BTreeSet<String>,
) -> Report {
    let (code, out) = guest_as(
        &vm.runtime,
        workspace,
        "agent",
        &logged(tool.name, tool.script),
        tool.limit,
    )
    .await;
    eprintln!("== {} (exit {code})\n{out}", tool.name);
    // The audit lags the tool by a moment.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let records = records_of(vm, workspace);
    let mut report = Report {
        name: tool.name.to_owned(),
        rc: word(&out, "RADAR step=", "rc=").to_owned(),
        suspicious: word(&out, "RADAR step=", "suspicious_lines=").to_owned(),
        said: out
            .lines()
            .filter(|l| l.starts_with("  "))
            .collect::<Vec<_>>()
            .join("\n"),
        by_set: BTreeSet::new(),
        by_rule: BTreeSet::new(),
        refused: BTreeSet::new(),
        open_rows: open_rows(vm, workspace)
            .into_iter()
            .filter(|row| reported.insert(row.clone()))
            .collect(),
    };
    for r in records.iter().skip(*seen) {
        let host = r["host"].as_str().unwrap_or("?").to_owned();
        if r["decision"] != "allow" {
            report.refused.insert(host);
        } else if r["rule_set"]
            .as_str()
            .is_some_and(|s| s.starts_with("builtin:"))
        {
            report.by_set.insert(host);
        } else {
            report.by_rule.insert(host);
        }
    }
    *seen = records.len();
    report
}

/// A started host and a workspace that has no rule of its own.
async fn bare_workspace(tag: &str) -> (VmHost, WorkspaceId, Api) {
    let vm = start_vm_host(tag).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace =
        create_workspace_from(&vm, &api, tag, &[], json!({"memory_mib": 6144, "cpus": 4})).await;
    (vm, workspace, api)
}

/// Switches a built-in set on for every workspace.
async fn switch_on(api: &Api, slug: &str) {
    let reply = api
        .put(
            &format!("/api/rule-sets/builtin:{slug}/switch"),
            &json!({"enabled": true}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{slug}: {}", reply.body);
}

/// Allows `host` in `workspace` only.
async fn allow(api: &Api, workspace: &WorkspaceId, host: &str) {
    let reply = api
        .post(
            "/api/rules",
            &json!({"scope": {"type": "workspace", "workspace": workspace.as_str()},
                    "pattern": host, "effect": "allow"})
            .to_string(),
        )
        .await;
    assert_eq!(reply.status, 201, "{host}: {}", reply.body);
}

/// The hosts outside the sets that `tool` needs.
fn outside(tool: &str) -> BTreeSet<String> {
    OUTSIDE_THE_SETS
        .iter()
        .filter(|(t, _)| *t == tool)
        .flat_map(|(_, hosts)| hosts.iter().map(|h| (*h).to_owned()))
        .collect()
}

/// Writes the project files into the guest.
async fn write_project(vm: &VmHost, workspace: &WorkspaceId) {
    let (code, out) = guest_as(
        &vm.runtime,
        workspace,
        "agent",
        PROJECT_FILES,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(code, 0, "writing the project files: {out}");
}

/// Leg 1: the three sets alone carry every package manager.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_package_sets_the_three_built_in_sets_alone_carry_every_package_manager() {
    let (vm, workspace, api) = bare_workspace("rps").await;
    for slug in SETS {
        switch_on(&api, slug).await;
    }
    for (_, hosts) in OUTSIDE_THE_SETS {
        for host in *hosts {
            allow(&api, &workspace, host).await;
        }
    }
    write_project(&vm, &workspace).await;

    let mut seen = 0;
    let mut reported = BTreeSet::new();
    let mut failures = Vec::new();
    let mut table = BTreeMap::new();
    for tool in tools() {
        let report = run_tool(&vm, &workspace, &tool, &mut seen, &mut reported).await;
        eprintln!(
            "RADAR hosts tool={} by_sets={:?} outside_the_sets={:?}",
            report.name, report.by_set, report.by_rule
        );
        if let Some(why) = report.failure(&outside(tool.name)) {
            failures.push(why);
        }
        table.insert(report.name.clone(), (report.by_set, report.by_rule));
    }
    eprintln!("RADAR expected hosts per tool: {OUTSIDE_THE_SETS:?}\nreached: {table:#?}");
    assert!(failures.is_empty(), "the radar: {failures:#?}");
    let end = open_rows(&vm, &workspace);
    assert!(end.is_empty(), "open inbox rows at the end: {end:?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The text a guest tool reads for a waiting host: a plain-HTTP request through the workspace's
/// proxy settings, the body of the refusal.
async fn pending_text(vm: &VmHost, workspace: &WorkspaceId, host: &str) -> String {
    let (_, out) = guest_as(
        &vm.runtime,
        workspace,
        "agent",
        &format!("curl -sS --max-time 20 http://{host}/ || true"),
        Duration::from_secs(60),
    )
    .await;
    out.trim().to_owned()
}

/// What leg 2 runs each round: it carries on from where the last round stopped.
const SMALL_CRATE_ROUND: &str = r#"
command -v cc >/dev/null || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends build-essential
[ -x ~/.cargo/bin/rustc ] || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
. ~/.cargo/env
mkdir -p ~/small/src
cat > ~/small/Cargo.toml <<'TOML'
[package]
name = "small"
version = "0.1.0"
edition = "2021"
[dependencies]
itoa = "1"
semver = "1"
TOML
cat > ~/small/src/lib.rs <<'RS'
pub fn digits(n: u64) -> String { itoa::Buffer::new().format(n).to_owned() }
#[cfg(test)]
mod tests {
    #[test]
    fn digits_and_versions() {
        assert_eq!(super::digits(1234), "1234");
        assert!(semver::Version::parse("1.2.3").unwrap() < semver::Version::parse("1.10.0").unwrap());
    }
}
RS
cd ~/small
cargo build
cargo test
"#;

/// Leg 2: no rules; each failure's pending text is read, approved through the API, and the step
/// runs again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_package_sets_with_no_rules_the_approve_and_retry_loop_ends_green() {
    let (vm, workspace, api) = bare_workspace("rpl").await;
    let tool = Tool {
        name: "small_crate",
        script: SMALL_CRATE_ROUND,
        limit: Duration::from_mins(20),
    };
    let mut seen = 0;
    let mut reported = BTreeSet::new();
    let mut approved: Vec<String> = Vec::new();
    let mut green = None;
    for round in 1..=MAX_ROUNDS {
        let report = run_tool(&vm, &workspace, &tool, &mut seen, &mut reported).await;
        if report.rc == "0" {
            assert!(
                report.open_rows.is_empty(),
                "green round {round} with open rows: {:?}",
                report.open_rows
            );
            green = Some(round);
            break;
        }
        assert!(
            !report.open_rows.is_empty(),
            "round {round} failed with nothing waiting for approval, so the failure is not an approval: {}",
            report.said
        );
        for row in &report.open_rows {
            let host = row.rsplit_once(':').unwrap().0;
            let text = pending_text(&vm, &workspace, host).await;
            eprintln!("RADAR round={round} pending host={host} text={text:?}");
            assert!(
                text.contains(host) && text.contains("waiting for approval in Puddle"),
                "the guest was not told the host waits for approval: {text:?}"
            );
        }
        let listed = api
            .get(&format!("/api/pending?workspace={}", workspace.as_str()))
            .await
            .json();
        for request in listed["requests"].as_array().unwrap() {
            let id = request["id"].as_i64().unwrap();
            let reply = api.post(&format!("/api/pending/{id}/approve"), "{}").await;
            // An approval of a host closes its other rows (port 80 and 443): those answer 409.
            let closed_already = reply.status == 409 && reply.body.contains("already allowed");
            assert!(
                reply.status == 200 || closed_already,
                "approving {request}: {}",
                reply.body
            );
            let host = request["host"].as_str().unwrap().to_owned();
            if !approved.contains(&host) {
                approved.push(host);
            }
        }
    }
    eprintln!("RADAR rounds={green:?} max={MAX_ROUNDS} approved={approved:?}");
    assert!(
        green.is_some(),
        "not green within {MAX_ROUNDS} rounds; approved so far: {approved:?}"
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Breakage: without `files.pythonhosted.org` in "Package registries" the pip step fails and the
/// report names the host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_package_sets_without_the_pypi_download_host_the_pip_step_fails_naming_it() {
    let (vm, workspace, api) = bare_workspace("rpb").await;
    for slug in SETS {
        switch_on(&api, slug).await;
    }
    let reply = api
        .put(
            "/api/rule-sets/builtin:package-registries/entry-switch",
            &json!({"pattern": "files.pythonhosted.org", "enabled": false}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    write_project(&vm, &workspace).await;
    let mut seen = 0;
    let mut reported = BTreeSet::new();
    let setup = tools()
        .into_iter()
        .filter(|t| t.name == "apt_setup")
        .chain(std::iter::once(Tool {
            name: "pip",
            script: PIP,
            limit: Duration::from_mins(10),
        }))
        .collect::<Vec<_>>();
    let mut last = None;
    for tool in &setup {
        let report = run_tool(&vm, &workspace, tool, &mut seen, &mut reported).await;
        last = Some(report);
    }
    let report = last.unwrap();
    let why = report
        .failure(&BTreeSet::new())
        .expect("the pip step passed although files.pythonhosted.org is off");
    eprintln!("RADAR breakage report: {why}");
    assert!(
        why.contains("files.pythonhosted.org"),
        "the report does not name the missing host: {why}"
    );

    // Not asserted: a pip that was still backing off when the report was read can hold the
    // guest past the graceful stop, and the test is about the report, not the stop.
    drop(vm.host.shutdown().await);
}
