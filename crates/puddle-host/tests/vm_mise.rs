// SPDX-License-Identifier: AGPL-3.0-or-later
//! Risk tests for the mise integration (docs/plan/mise-build-plan.md, M-1): the real `mise` binary
//! in a real microVM, reaching the real internet through Puddle's proxy. It answers what the design
//! left assumed: the musl binary trusts Puddle's CA, two installs of one tool at once are safe, a
//! full install makes no connection of its own, no `mise.lock` appears in a repository that has
//! none, and which hosts node, python, go and a Temurin JDK reach.
//!
//! The connections are read from the audit log of the workspace, so a count is what the proxy saw,
//! not what the tool says. A host that no rule allows is not silently approved: the proxy refuses
//! it, the step fails and the message names the open inbox rows. The test is named `vm_radar_*`
//! (live third parties: mise's site, GitHub, nodejs.org, go.dev, Adoptium), so the per-pull-request
//! jobs skip it and the weekly `full-test` workflow runs it.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; what each step said goes to the log"
)]

mod support;
mod vm_support;

use std::collections::BTreeMap;
use std::time::Duration;

use puddle_e2e::lab::WebOptions;
use serde_json::json;
use support::{Api, named_id_options};
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host_platform, web_internet};

/// The mise release the build plan pins (M-3) and the checksum of its musl tarball, from
/// `https://mise.jdx.dev/v2026.10.21/SHASUMS256.txt`.
const MISE_VERSION: &str = "v2026.10.21";
const MISE_SHA256: &str = "63b5621467b5b9950b99b7333aec1b4a34c5f23a9e464d330357c5b35502e8d7";

/// The hosts the tools reach (the audit these produce is the host list for the "mise tools" set).
const HOSTS: &[&str] = &[
    // mise's site (the pinned download), its version index and its Java metadata.
    "mise.jdx.dev",
    "mise-versions.jdx.dev",
    "mise-java.jdx.dev",
    // GitHub release downloads (python-build-standalone, Temurin).
    "github.com",
    "release-assets.githubusercontent.com",
    // Node, Go, and the Sigstore trust root mise checks GitHub attestations against.
    "nodejs.org",
    "dl.google.com",
    "tuf-repo-cdn.sigstore.dev",
];

/// The environment that points mise at a data directory of its own under `$HOME/<dir>`.
fn dirs(dir: &str) -> String {
    format!(
        "export MISE_DATA_DIR=$HOME/{dir}/data MISE_CACHE_DIR=$HOME/{dir}/cache MISE_STATE_DIR=$HOME/{dir}/state; "
    )
}

/// Every connection of the workspace in the audit log, as `host [decision]` to its count.
async fn connections(api: &Api, workspace: &str) -> BTreeMap<String, usize> {
    // The audit log is written a moment after the connection.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let page = api
        .get(&format!(
            "/api/audit?workspace={workspace}&type=connection&limit=500"
        ))
        .await
        .json();
    let mut tally = BTreeMap::new();
    for entry in page.get("entries").and_then(|e| e.as_array()).unwrap() {
        let record = entry.get("record").unwrap();
        let host = record.get("host").and_then(|h| h.as_str()).unwrap_or("?");
        let decision = record
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("?");
        *tally.entry(format!("{host} [{decision}]")).or_insert(0) += 1;
    }
    tally
}

/// What `after` has that `before` does not: the connections one step made.
fn made_between(
    before: &BTreeMap<String, usize>,
    after: &BTreeMap<String, usize>,
) -> BTreeMap<String, usize> {
    after
        .iter()
        .filter_map(|(host, n)| {
            let d = n - before.get(host).copied().unwrap_or(0);
            (d > 0).then(|| (host.clone(), d))
        })
        .collect()
}

fn field<'a>(text: &'a str, key: &str) -> &'a str {
    text.lines()
        .rev()
        .find_map(|l| l.strip_prefix(key))
        .unwrap_or("?")
}

/// Runs `script` as the session user and returns its exit code and output (the log gets both).
#[expect(
    clippy::print_stderr,
    reason = "the test log: what each guest step said, read when a step fails"
)]
async fn run(
    vm: &VmHost,
    ws: &puddle_types::WorkspaceId,
    name: &str,
    script: &str,
) -> (i32, String) {
    let (code, out) = guest_as(&vm.runtime, ws, "agent", script, Duration::from_mins(20)).await;
    eprintln!("== {name} (exit {code})\n{out}");
    (code, out)
}

/// The open inbox rows of the workspace: hosts a step asked for that no rule allows.
fn asked_for(vm: &VmHost, ws: &puddle_types::WorkspaceId) -> Vec<String> {
    vm.host
        .store()
        .open_pending_every_user(Some(ws))
        .unwrap()
        .iter()
        .map(|r| format!("{}:{}", r.host, r.port))
        .collect()
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_mise_through_the_proxy() {
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("mise", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let ws = create_workspace_from(&vm, &api, "mise", HOSTS, json!({})).await;
    let name = format!("{}-mise", vm.settings.prefix.as_str());
    let m = "$HOME/mise-dl/mise/bin/mise";

    // The pinned download, as the build plan's M-3 does it: curl over the proxy, the hash checked.
    let (code, out) = run(
        &vm,
        &ws,
        "download",
        &format!(
            "set -e; mkdir -p $HOME/mise-dl && cd $HOME/mise-dl \
             && curl -fsSL -o mise.tgz https://mise.jdx.dev/{MISE_VERSION}/mise-{MISE_VERSION}-linux-x64-musl.tar.gz \
             && echo '{MISE_SHA256}  mise.tgz' | sha256sum -c - \
             && tar xzf mise.tgz && ./mise/bin/mise --version; \
             echo \"ssl_cert_file=$SSL_CERT_FILE\""
        ),
    )
    .await;
    assert!(code == 0 && out.contains("mise.tgz: OK"), "{out}");
    eprintln!(
        "connections after the download: {:?}",
        connections(&api, &name).await
    );

    // (1) The binary trusts the Puddle CA: a TLS request through the proxy works with the
    // environment the sandbox gives every process. The control, with every certificate variable
    // unset, shows whether the system store would have done it anyway.
    let (_, out) = run(
        &vm,
        &ws,
        "ca",
        &format!(
            "{d} echo \"with=$({m} ls-remote node 2>&1 | tail -1)\"; \
             echo \"with_rc=$({m} ls-remote node >/dev/null 2>&1; echo $?)\"; \
             echo \"without_rc=$(env -u SSL_CERT_FILE -u CURL_CA_BUNDLE -u REQUESTS_CA_BUNDLE -u GIT_SSL_CAINFO {m} ls-remote --fresh node >/dev/null 2>&1; echo $?)\"",
            d = dirs("m1")
        ),
    )
    .await;
    assert_eq!(
        field(&out, "with_rc="),
        "0",
        "mise does not trust the CA: {out}"
    );
    eprintln!(
        "CA control (without the variables): rc={}",
        field(&out, "without_rc=")
    );

    // (2) Two installs of one tool at once, an empty data directory.
    let before = connections(&api, &name).await;
    let (_, out) = run(
        &vm,
        &ws,
        "concurrent",
        &format!(
            "{d} v=$({m} ls-remote node@22 | tail -1); echo \"version=$v\"; \
             ({m} install node@$v >$HOME/m2-a.log 2>&1; echo $? >$HOME/m2-a.rc) & \
             ({m} install node@$v >$HOME/m2-b.log 2>&1; echo $? >$HOME/m2-b.rc) & wait; \
             echo \"rc_a=$(cat $HOME/m2-a.rc)\"; echo \"rc_b=$(cat $HOME/m2-b.rc)\"; \
             echo \"installs=$(find $MISE_DATA_DIR/installs/node -mindepth 1 -maxdepth 1 -type d -printf '%f ')\"; \
             echo \"works=$({m} exec node@$v -- node --version)\"; \
             echo '--- a'; tail -5 $HOME/m2-a.log; echo '--- b'; tail -5 $HOME/m2-b.log; \
             echo \"lockwords=$(cat $HOME/m2-a.log $HOME/m2-b.log | grep -ci 'lock')\"",
            d = dirs("m2")
        ),
    )
    .await;
    let two = made_between(&before, &connections(&api, &name).await);
    eprintln!("connections of the two concurrent installs: {two:?}");
    assert_eq!(field(&out, "rc_a="), "0", "{out}");
    assert_eq!(field(&out, "rc_b="), "0", "{out}");
    let version = field(&out, "version=").to_owned();
    assert_eq!(
        field(&out, "installs=").trim(),
        version,
        "one install, not two: {out}"
    );
    assert_eq!(field(&out, "works="), format!("v{version}"), "{out}");

    // (3) A full install makes no connection: the tool of step 2 is present and pinned exactly.
    // (4) In a repository with no `mise.lock`, `mise install` leaves `git status` clean. The
    // project is a git checkout, so the same run answers both.
    let before = connections(&api, &name).await;
    let (_, out) = run(
        &vm,
        &ws,
        "full-install",
        &format!(
            "{d} mkdir $HOME/proj && cd $HOME/proj && git init -q . \
             && git config user.email a@b.c && git config user.name t \
             && printf '[tools]\\nnode = \"{version}\"\\n' > mise.toml && git add mise.toml && git commit -qm init; \
             {m} trust -q; {m} install; echo \"rc=$?\"; \
             echo \"status=[$(git status --porcelain)]\"; echo \"lockfile=$(ls mise.lock 2>&1 | head -1)\"; \
             echo \"lockfile_setting=$({m} settings get lockfile)\"; \
             echo \"second_rc=$({m} install >/dev/null 2>&1; echo $?)\"",
            d = dirs("m2")
        ),
    )
    .await;
    let silent = made_between(&before, &connections(&api, &name).await);
    eprintln!("connections of a full install: {silent:?}");
    assert_eq!(field(&out, "rc="), "0", "{out}");
    assert!(
        silent.is_empty(),
        "a full install made connections: {silent:?}\n{out}"
    );
    assert_eq!(
        field(&out, "status=").trim(),
        "[]",
        "a repository changed: {out}"
    );

    // (4, with work to do) the same repository, an empty data directory: the install really
    // downloads, and still leaves no lockfile.
    let (_, out) = run(
        &vm,
        &ws,
        "install-with-work",
        &format!(
            "{d} cd $HOME/proj && {m} install; echo \"rc=$?\"; \
             echo \"status=[$(git status --porcelain)]\"; echo \"lockfile=$(ls mise.lock 2>&1 | head -1)\"; \
             echo \"lockfiles_elsewhere=$(find $HOME/m4 -name 'mise.lock' | head -3)\"",
            d = dirs("m4")
        ),
    )
    .await;
    assert_eq!(field(&out, "rc="), "0", "{out}");
    assert_eq!(
        field(&out, "status=").trim(),
        "[]",
        "an install with work left a file in the repository: {out}"
    );

    // (5) The hosts each tool reaches, one fresh data directory per tool, from the audit.
    let mut per_tool = BTreeMap::new();
    for (tool, spec, check) in [
        ("node", "node@22", "node --version"),
        ("python", "python@3.13", "python --version"),
        ("go", "go@latest", "go version"),
        ("java", "java@temurin-21", "java -version"),
    ] {
        let before = connections(&api, &name).await;
        let (code, out) = run(
            &vm,
            &ws,
            &format!("hosts-{tool}"),
            &format!(
                "{d} cd $HOME && {m} install {spec}; echo \"rc=$?\"; \
                 echo \"works=$({m} exec {spec} -- {check} 2>&1 | head -1)\"",
                d = dirs(&format!("h-{tool}"))
            ),
        )
        .await;
        let reached = made_between(&before, &connections(&api, &name).await);
        eprintln!("hosts of {tool}: {reached:?}");
        assert!(
            code == 0 && field(&out, "rc=") == "0",
            "{tool} did not install (hosts asked for and not allowed: {:?}): {out}",
            asked_for(&vm, &ws)
        );
        per_tool.insert(tool, reached);
    }
    eprintln!("HOSTS PER TOOL: {per_tool:#?}");
    assert!(
        asked_for(&vm, &ws).is_empty(),
        "a host was asked for that no rule allows: {:?}",
        asked_for(&vm, &ws)
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The release `ci/mise.sha256` pins, which `puddle-agent mise-fetch` downloads.
fn pinned_version() -> &'static str {
    include_str!("../../../ci/mise.sha256")
        .lines()
        .find(|l| !l.starts_with('#') && !l.trim().is_empty())
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
}

/// The `tools:<workspace>` problem's detail, once Puddle has raised it.
async fn tools_problem(api: &Api, ws: &puddle_types::WorkspaceId) -> Option<String> {
    let key = format!("tools:{ws}");
    let reply = api.get("/api/problems").await.json();
    reply
        .get("problems")?
        .as_array()?
        .iter()
        .find(|p| p.get("key").and_then(|k| k.as_str()) == Some(key.as_str()))
        .map(|p| {
            p.get("detail")
                .and_then(|d| d.as_str())
                .unwrap_or_default()
                .to_owned()
        })
}

/// A fresh workspace with no rule of its own downloads the pinned mise by itself: System managed
/// covers `mise.jdx.dev`, so no inbox row is opened, and `bin/mise --version` names the pin.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_a_fresh_workspace_gets_the_pinned_mise_without_an_inbox_row() {
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("misefetch", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let ws = create_workspace_from(&vm, &api, "misefetch", &[], json!({})).await;
    let want = pinned_version();
    // The download runs in the background after the start: wait for the binary.
    let said = puddle_test_kit::eventually_async(
        "the pinned mise on the volume",
        Duration::from_secs(90),
        || async {
            let (code, out) = run(
                &vm,
                &ws,
                "version",
                &format!(
                    "/workspaces/{ws}/.puddle/mise/bin/mise --version 2>&1; \
                     test -f /workspaces/{ws}/.puddle/mise/bin/.puddle-mise && echo marker"
                ),
            )
            .await;
            (code == 0 && out.contains("marker")).then_some(out)
        },
    )
    .await;
    assert!(
        said.contains(want),
        "mise --version does not print {want}: {said:?}"
    );
    assert!(
        asked_for(&vm, &ws).is_empty(),
        "an inbox row was opened: {:?}",
        asked_for(&vm, &ws)
    );
    assert_eq!(tools_problem(&api, &ws).await, None);
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// An own deny rule on `mise.jdx.dev` wins over System managed: the start still ends `Done`, the
/// workspace has no mise, and the problem names the rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_an_own_deny_on_the_mise_host_gives_a_problem_and_a_done_start() {
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("misedeny", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let id = puddle_types::WorkspaceId::new(&format!("{}-misedeny", vm.settings.prefix.as_str()))
        .unwrap();
    vm.host
        .store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&puddle_store::NewRule {
            scope: puddle_store::Scope::Workspace(id.clone()),
            pattern: puddle_store::Pattern::parse("mise.jdx.dev").unwrap(),
            effect: puddle_store::Effect::Deny,
            expires_at: None,
            created_by: puddle_store::Actor::Cli,
        })
        .unwrap();
    // `create_workspace_from` returns once the start ended `Done`.
    let ws = create_workspace_from(&vm, &api, "misedeny", &[], json!({})).await;
    assert_eq!(ws, id);
    let detail =
        puddle_test_kit::eventually_async("the tools problem", Duration::from_secs(90), || {
            tools_problem(&api, &ws)
        })
        .await;
    assert!(detail.contains("deny mise.jdx.dev"), "{detail}");
    assert!(detail.contains("works without them"), "{detail}");
    let (_, out) = run(
        &vm,
        &ws,
        "no-binary",
        &format!("test -e /workspaces/{ws}/.puddle/mise/bin/mise && echo present || echo absent"),
    )
    .await;
    assert!(out.contains("absent"), "{out}");
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

const NODE_PIN: &str = "22.23.5";
const JAVA_PIN: &str = "temurin-21.0.13+9.0.LTS";

/// Matches the `workspace_tools` event of `ws` in `phase`.
fn tools_in(
    ws: &puddle_types::WorkspaceId,
    phase: &'static str,
) -> impl Fn(&serde_json::Value) -> bool {
    let ws = ws.to_string();
    move |e| {
        e.get("type").and_then(|t| t.as_str()) == Some("workspace_tools")
            && e.get("workspace").and_then(|w| w.as_str()) == Some(ws.as_str())
            && e.pointer("/state/phase").and_then(|p| p.as_str()) == Some(phase)
    }
}

/// Stops and starts `ws`, returning once the start ended.
async fn restart(api: &Api, events: &mut support::Events, ws: &puddle_types::WorkspaceId) {
    api.post(&format!("/api/workspaces/{ws}/stop"), "").await;
    events
        .until(support::ended(ws.as_str()), Duration::from_mins(2))
        .await;
    api.post(&format!("/api/workspaces/{ws}/start"), "").await;
    let end = events
        .until(support::ended(ws.as_str()), Duration::from_mins(5))
        .await;
    assert_eq!(
        end.get("step").and_then(|s| s.as_str()),
        Some("done"),
        "{end}"
    );
}

/// Plan M-5: after a start Puddle installs what the root `mise.toml` asks for (node and a JDK),
/// in the background, reporting progress until "ready"; the JDK trusts Puddle's CAs; a second
/// start finds nothing missing, runs no install and makes no connection to a tool host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_tools_install_after_the_start_and_a_second_start_installs_nothing() {
    let net = web_internet(&[], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("miseinst", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let ws = create_workspace_from(&vm, &api, "miseinst", HOSTS, json!({})).await;
    // Nothing asks for a tool yet: the download, then "ready" with no install.
    events
        .until(tools_in(&ws, "ready"), Duration::from_mins(3))
        .await;
    let (code, out) = run(
        &vm,
        &ws,
        "config",
        &format!(
            "printf '[tools]\\nnode = \"{NODE_PIN}\"\\njava = \"{JAVA_PIN}\"\\n' > /workspaces/{ws}/mise.toml && cat /workspaces/{ws}/mise.toml"
        ),
    )
    .await;
    assert_eq!(code, 0, "{out}");

    restart(&api, &mut events, &ws).await;
    let installing = events
        .until(tools_in(&ws, "installing"), Duration::from_mins(2))
        .await;
    assert_eq!(installing["state"]["total"], 2, "{installing}");
    events
        .until(tools_in(&ws, "ready"), Duration::from_mins(15))
        .await;
    assert_eq!(tools_problem(&api, &ws).await, None);
    let (code, out) = run(
        &vm,
        &ws,
        "node",
        &format!("cd /workspaces/{ws} && node --version"),
    )
    .await;
    assert!(code == 0 && out.contains(&format!("v{NODE_PIN}")), "{out}");
    // The trust step ran for the new JDK: its cacerts holds Puddle's CAs.
    let (code, out) = run(
        &vm,
        &ws,
        "jdk-trust",
        &format!(
            "k=$(ls /workspaces/{ws}/.puddle/mise/data/installs/java/*/bin/keytool | head -1); \
             echo \"puddle_cas=$(\"$k\" -list -cacerts -storepass changeit 2>/dev/null | grep -c '^puddle-')\""
        ),
    )
    .await;
    assert_eq!(code, 0, "{out}");
    assert_ne!(
        field(&out, "puddle_cas="),
        "0",
        "the JDK does not trust Puddle's CAs: {out}"
    );
    assert!(asked_for(&vm, &ws).is_empty(), "{:?}", asked_for(&vm, &ws));

    let name = ws.to_string();
    let before = connections(&api, &name).await;
    let seen = events.seen.len();
    restart(&api, &mut events, &ws).await;
    events
        .until(tools_in(&ws, "ready"), Duration::from_mins(3))
        .await;
    let since: Vec<_> = events
        .seen
        .iter()
        .skip(seen)
        .filter(|e| tools_in(&ws, "installing")(e))
        .collect();
    assert!(since.is_empty(), "the second start installed: {since:?}");
    let made = made_between(&before, &connections(&api, &name).await);
    let tool_hosts: Vec<_> = made
        .keys()
        .filter(|h| HOSTS.iter().any(|t| h.starts_with(t)))
        .collect();
    assert!(
        tool_hosts.is_empty(),
        "the second start reached tool hosts: {made:?}"
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
