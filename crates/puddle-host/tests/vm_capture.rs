// SPDX-License-Identifier: GPL-3.0-or-later
//! Transparent capture on a real microVM (K on Linux KVM, W on Windows WHP), with no proxy
//! setting in play: the host's boot hook turns capture on, and tools that ignore proxy settings
//! reach the internet only the way a rule allows, by name or by address, through the agent and
//! the proxy.
//!
//! The first test is the battery from the spike's list that needs only what the workspace image
//! ships (`curl`, `bash`, `ssh`, `xargs`): a scrubbed environment, raw TCP, a direct address (a
//! deny row, then an approval and a retry with no restart), UDP and IPv6 failing at once, the
//! stub's answers, SSH refused with the message, 256 parallel fetches. The second is the
//! hostile one: a root process that deletes the capture rules gets nothing through, and the
//! agent puts them back.
//!
//! The third test is the Python half: `python3` and `pip` with an empty environment, against the
//! fake website and a fake package index.
//!
//! The sites the tests reach are a fake website behind a fake company proxy (`web_internet`), so no
//! result here depends on a public site being up. The packages the tests bring (`nft`, `pip`) are
//! saved from the real mirror on the first run and served from the fake website after that
//! (`vm_support::PackageKit`), so a warm cache needs no package mirror; the cold run names the mirror
//! when it fails. One test keeps a live third party on purpose: the third runs the real `apt-get update`
//! against Debian's mirrors (the assertion is about apt's own behaviour with Debian's `SRV` records,
//! which a stand-in mirror would not carry).
//!
//! Every guest command runs as root through the runtime's exec, which carries the workspace's
//! proxy settings; the probes that must not use them say `env -i` or `--noproxy '*'`.
#![expect(
    clippy::unwrap_used,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test; what the guest said goes to the log"
)]

mod support;
mod vm_support;

use std::time::Duration;

use puddle_e2e::lab::{WebOptions, pip_index};
use puddle_store::{Actor, Resolution};
use puddle_test_kit::{eventually, eventually_async};
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, named_id_options};
use vm_support::{
    Flavour, KIT_HOST, PROBE_IMAGE, PackageKit, VmHost, WebInternet, allowed_connections, as_agent,
    container_fetch, create_workspace_from, field, guest, guest_as, guest_live, reached_by_host,
    start_vm_host, start_vm_host_platform, wait_for_connection_record, web_internet,
};

/// The fake website's name (it answers plain HTTP and HTTPS).
const SITE: &str = "site.invalid";

/// The fake package index's name, and the one project it lists.
const INDEX: &str = "index.invalid";
const PROJECT: &str = "puddle-probe";

/// Creates a workspace on `host` whose sandbox may reach `allowed` and returns its name.
async fn create_workspace(vm: &VmHost, api: &Api, tag: &str, allowed: &[&str]) -> WorkspaceId {
    create_workspace_from(vm, api, tag, allowed, json!({"memory_mib": 1024})).await
}

/// A guest script's clock: `now` prints the milliseconds since the epoch.
const CLOCK: &str = "now() { local t=${EPOCHREALTIME/[.,]/}; echo $((t / 1000)); }";

/// `script` as a command that runs it with bash and an empty stdin (so no tool in it reads the
/// script), after [`CLOCK`], and always ends well: the test reads what it printed.
fn bash_script(script: &str) -> String {
    format!(
        "cat > /tmp/probe.sh <<'PUDDLE_EOF'\n{CLOCK}\n{script}\ntrue\nPUDDLE_EOF\nbash /tmp/probe.sh </dev/null"
    )
}

/// The open inbox rows of `workspace`, as `host:port`.
fn open_rows(vm: &VmHost, workspace: &WorkspaceId) -> Vec<String> {
    vm.host
        .store()
        .open_pending_every_user(Some(workspace))
        .unwrap()
        .iter()
        .map(|r| format!("{}:{}", r.host, r.port))
        .collect()
}

/// Waits for the open row of `workspace` for `wanted` and returns its id.
async fn wait_for_row(
    vm: &VmHost,
    workspace: &WorkspaceId,
    wanted: &str,
) -> puddle_types::PendingId {
    eventually(
        &format!("an open row for {wanted}"),
        Duration::from_secs(10),
        || {
            let rows = vm
                .host
                .store()
                .open_pending_every_user(Some(workspace))
                .unwrap();
            rows.iter()
                .find(|r| r.host.to_string() == wanted)
                .map(|row| row.id)
                .ok_or_else(|| format!("{:?}", open_rows(vm, workspace)))
        },
    )
    .await
}

/// With capture on from the boot and no proxy setting in the command, tools that never look at
/// one still work for what a rule allows and fail at once for the rest.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_capture_reaches_what_a_rule_allows_with_no_proxy_setting_and_refuses_the_rest_at_once()
{
    let net = web_internet(&[SITE], WebOptions::default(), named_id_options()).await;
    // The address nothing allows at first is answered by the same site once a rule lets it through.
    net.proxy.route("1.1.1.1:443", net.web.tls_addr());
    let vm = start_vm_host_platform("capt", net.options, &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace(&vm, &api, "capt", &[SITE]).await;
    let run = |script: String| {
        let (runtime, workspace) = (&vm.runtime, &workspace);
        async move { guest(runtime, workspace, &bash_script(&script)).await }
    };

    // The boot wrote the stub's line first and kept the others.
    let resolv = run("head -1 /etc/resolv.conf; echo; cat /etc/resolv.conf".into()).await;
    eprintln!("{resolv}");
    assert!(resolv.starts_with("nameserver 198.18.0.1\n"), "{resolv}");

    // A tool with an empty environment: the name resolves to a stand-in through the stub, the
    // connection is redirected, and the host sees `CONNECT example.com:443`.
    let before = allowed_connections(&vm, SITE, 443);
    let scrubbed = run(format!(
        "env -i PATH=/usr/bin:/bin curl -sS -D - -o /dev/null --max-time 30 \
         -w 'http=%{{http_code}} ip=%{{remote_ip}}\\n' https://{SITE}/ 2>&1; echo \"exit=$?\""
    ))
    .await;
    eprintln!("{scrubbed}");
    assert_eq!(field(&scrubbed, "http="), "200", "{scrubbed}");
    assert!(field(&scrubbed, "ip=").starts_with("198.1"), "{scrubbed}");
    wait_for_connection_record(&vm, SITE, 443, before).await;
    assert!(
        reached_by_host(&net.web, SITE) > 0,
        "the site did not see the capture's request: {:?}",
        net.web.seen()
    );

    // Raw TCP: bash's /dev/tcp, no HTTP library, port 80.
    let raw = run(format!(
        "exec 3<>/dev/tcp/{SITE}/80 && printf 'GET / HTTP/1.0\\r\\nHost: {SITE}\\r\\n\\r\\n' >&3 \
         && IFS= read -r -t 20 line <&3; echo \"first-line=${{line%$'\\r'}}\"; \
         while IFS= read -r -t 5 more <&3 && [ \"$more\" != $'\\r' ]; do echo \"header: ${{more%$'\\r'}}\"; done"
    ))
    .await;
    eprintln!("{raw}");
    assert!(raw.contains("first-line=HTTP/1."), "{raw}");

    // A client that does use the proxy settings still works, and the stub answers lookups for
    // names nothing allows without any inbox row (the connect is what asks).
    let proxied = run(format!(
        "curl -sS -D - -o /dev/null --max-time 30 -w 'http=%{{http_code}}\\n' https://{SITE}/ 2>&1"
    ))
    .await;
    assert_eq!(field(&proxied, "http="), "200", "{proxied}");
    // flake-ok: no rule allows this name, so the host never looks it up (the stub answers at once)
    let denied_name = run("getent hosts puddle-nothing-allows-this.example.net".into()).await;
    eprintln!("{denied_name}");
    assert!(denied_name.starts_with("198.1"), "{denied_name}");
    assert!(
        !open_rows(&vm, &workspace)
            .iter()
            .any(|r| r.contains("puddle-nothing-allows-this")),
        "a lookup alone made an inbox row: {:?}",
        open_rows(&vm, &workspace)
    );

    // Rerun of the network case: a direct address nothing allows is a deny row, never a way around the proxy.
    // Approve it, and the retry works with no restart.
    let direct = |label: &str| {
        run(format!(
            "s=$(now); curl --noproxy '*' -sS -k -D - -o /dev/null --connect-timeout 10 --max-time 20 \
             -w 'http=%{{http_code}}\\n' https://1.1.1.1/ 2>&1; echo \"{label} exit=$? ms=$(($(now) - s))\""
        ))
    };
    let refused = direct("first").await;
    eprintln!("{refused}");
    assert!(!refused.contains("exit=0 "), "{refused}");
    assert!(
        field(&refused, "ms=").parse::<u64>().unwrap() < 5000,
        "{refused}"
    );
    let id = wait_for_row(&vm, &workspace, "1.1.1.1").await;
    vm.host
        .store()
        .resolve_pending(id, &Resolution::allow(), Actor::Cli)
        .unwrap();
    let approved = direct("retry").await;
    eprintln!("{approved}");
    assert!(approved.contains("retry exit=0 "), "{approved}");
    assert_ne!(field(&approved, "http="), "000", "{approved}");
    assert!(
        reached_by_host(&net.web, "1.1.1.1") > 0,
        "the approved address did not reach the site: {approved}\n{:?}",
        net.web.seen()
    );

    // UDP (not DNS) and IPv6 literals fail at once, with a refusal and not a timeout.
    let fast = run(
        r#"for t in 1.1.1.1 192.0.2.5 198.18.0.9; do
             s=$(now); if (echo x > /dev/udp/$t/123) 2>/dev/null; then r=sent; else r=failed; fi
             echo "udp $t $r ms=$(($(now) - s))"
           done
           s=$(now)
           if timeout 10 bash -c 'echo > /dev/tcp/2606:4700:4700::1111/443' 2>/dev/null; then r=connected; else r=failed; fi
           echo "v6 $r ms=$(($(now) - s))""#
            .into(),
    )
    .await;
    eprintln!("{fast}");
    for line in fast.lines().filter(|l| l.starts_with("udp ")) {
        assert!(line.contains(" failed "), "{fast}");
        assert!(field(line, "ms=").parse::<u64>().unwrap() < 100, "{fast}");
    }
    let v6 = fast.lines().find(|l| l.starts_with("v6 ")).unwrap();
    assert!(v6.contains(" failed "), "{fast}");
    assert!(field(v6, "ms=").parse::<u64>().unwrap() < 1000, "{fast}");

    // SSH from a client that ignores ssh_config reaches Puddle's own server for a Git host, which
    // the rules decide as they decide the host's HTTPS: no rule allows github.com here, so the
    // client is refused with Puddle's message and no SSH row is asked for. The message goes out as
    // a line ahead of the identification string; OpenSSH shows such lines only at debug level
    // (`-v`), and at the default level says just "Connection closed".
    let ssh = run(
        "ssh -v -F /dev/null -o BatchMode=yes -o StrictHostKeyChecking=no -o ConnectTimeout=20 \
         -T git@github.com 2>&1; echo \"exit=$?\""
            .into(),
    )
    .await;
    eprintln!("{ssh}");
    assert!(ssh.contains("puddle: "), "{ssh}");
    assert!(
        !open_rows(&vm, &workspace)
            .iter()
            .any(|r| r.ends_with(":22")),
        "a captured SSH client made an inbox row: {:?}",
        open_rows(&vm, &workspace)
    );

    // 256 fetches in parallel from a scrubbed environment: none fails, none stalls. Each line
    // says where the time went (lookup, connect, first byte), so a slow one names its stage.
    let parallel = run(format!(
        "mkdir -p /tmp/heads
         seq 256 | xargs -P 32 -I{{}} env -i PATH=/usr/bin:/bin curl -sS -D /tmp/heads/{{}}.txt -o /dev/null \
         --max-time 30 -w '%{{http_code}} %{{time_total}} lookup=%{{time_namelookup}} connect=%{{time_connect}} first_byte=%{{time_starttransfer}}\n' \
         http://{SITE}/ > /tmp/parallel.txt 2>&1
         echo \"ok=$(grep -c '^200 ' /tmp/parallel.txt) lines=$(wc -l < /tmp/parallel.txt) \
         slowest=$(awk '{{print $2}}' /tmp/parallel.txt | sort -n | tail -1)\"
         sort -k2 -n /tmp/parallel.txt | tail -3
         grep -v '^200 ' /tmp/parallel.txt | head -5
         grep -L '^HTTP/1.1 200' /tmp/heads/*.txt | head -2 | xargs -r head -20"
    ))
    .await;
    eprintln!("{parallel}");
    assert_eq!(field(&parallel, "ok="), "256", "{parallel}");
    // Healthy, the slowest of the 256 takes well under 0.3 s (measured over 5,000 fetches on a
    // loaded 40-core host, 0.18 s at most). The budget is 5 s because that is the scale at which a
    // lost step shows: the stub's lookup timeout and a resolver's first retry are both 5 s, so a
    // fetch that waited on one lands just above it. Nothing slower than that is slowness.
    let slowest: f64 = field(&parallel, "slowest=").parse().unwrap();
    assert!(slowest < 5.0, "a fetch took {slowest} s: {parallel}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Capture is not enforcement, so a root process that deletes the rules gets nothing new
/// through (the VM has no route of its own), and the agent puts the rules back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_capture_survives_a_root_that_deletes_the_rules() {
    let kit = PackageKit::open(
        Flavour::Apt,
        puddle_api::DEFAULT_IMAGE,
        "nftables",
        "deb.debian.org and security.debian.org",
    );
    let net = web_internet(
        &[SITE, KIT_HOST],
        kit.options(WebOptions::default()),
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("flush", net.options, &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace(
        &vm,
        &api,
        "flush",
        &[SITE, KIT_HOST, "deb.debian.org", "security.debian.org"],
    )
    .await;
    let run = |script: String| {
        let (runtime, workspace) = (&vm.runtime, &workspace);
        async move { guest(runtime, workspace, &bash_script(&script)).await }
    };

    // The image ships no nft; the test brings one, through the proxy, as any tool would: from
    // the saved package files on a warm cache, from Debian's mirrors on the first run.
    let installed = kit.install(&vm, &workspace, "command -v nft").await;
    assert!(
        installed.trim_end().ends_with("/nft"),
        "nftables is not usable: {installed}"
    );

    for attack in ["nft flush ruleset", "nft delete table inet puddle"] {
        let out = run(format!(
            "nft list table inet puddle >/dev/null 2>&1 && echo before=present || echo before=absent
             {attack}
             echo \"after-attack=$(nft list table inet puddle >/dev/null 2>&1 && echo present || echo absent)\"
             curl --noproxy '*' -sS -k -D - -o /dev/null --connect-timeout 3 --max-time 8 \
               -w 'http=%{{http_code}}\\n' http://1.0.0.1/ 2>&1
             for i in 1 2 3 4 5 6 7 8 9 10; do
               nft list table inet puddle >/dev/null 2>&1 && break
               sleep 0.5
             done
             echo \"restored=$(nft list table inet puddle >/dev/null 2>&1 && echo yes || echo no)\"
             env -i PATH=/usr/bin:/bin curl -sS -D - -o /dev/null --max-time 30 \
               -w 'again http=%{{http_code}}\\n' http://{SITE}/ 2>&1"
        ))
        .await;
        eprintln!("{attack}: {out}");
        assert!(out.contains("before=present"), "{attack}: {out}");
        // Whether the agent had put the rules back by the time curl ran or not, an address no
        // rule allows never answered: it is a deny row or no route.
        assert_eq!(field(&out, "http="), "000", "{attack}: {out}");
        assert!(out.contains("restored=yes"), "{attack}: {out}");
        assert!(out.contains("again http=200"), "{attack}: {out}");
    }
    assert!(
        allowed_connections(&vm, "1.0.0.1", 80) == 0,
        "a connection to 1.0.0.1 was allowed"
    );

    // The agent told the host, and the user can see it: a counted problem on this workspace.
    let key = format!("capture-tamper:{workspace}");
    let listed = eventually_async(
        "the host lists the capture tamper problem",
        Duration::from_secs(60),
        || async {
            let body = api.get("/api/problems").await.body;
            body.contains(&key).then_some(body)
        },
    )
    .await;
    assert!(
        listed.contains("deleted Puddle's capture rules; Puddle put them back"),
        "{listed}"
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// apt with neither its proxy file nor a proxy variable: the stub resolves its names and the
/// capture carries its connections. apt asks for `_http._tcp.<mirror>` `SRV` records first and
/// connects to their target; Debian's point at a CDN name the user never allowed, and a stand-in
/// accepts the connect before the proxy refuses it, so apt would never fall back to the mirror's
/// own name. The host leaves such a record out of the answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_capture_apt_update_works_with_no_proxy_file_and_no_proxy_setting() {
    let vm = start_vm_host("aptn").await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace(
        &vm,
        &api,
        "aptn",
        &["deb.debian.org", "security.debian.org"],
    )
    .await;
    let before = allowed_connections(&vm, "deb.debian.org", 80);
    let out = guest_live(
        &vm.runtime,
        &workspace,
        &bash_script(
            "rm -f /etc/apt/apt.conf.d/99puddle-proxy; rm -rf /var/lib/apt/lists/*
             env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin apt-get update 2>&1 | tail -20
             echo \"apt=${PIPESTATUS[0]}\"",
        ),
        "deb.debian.org and security.debian.org",
    )
    .await;
    eprintln!("{out}");
    assert_eq!(
        field(&out, "apt="),
        "0",
        "apt-get update failed (live third party: deb.debian.org, security.debian.org): {out}"
    );
    assert!(
        !out.contains("Err:") && !out.contains("W: "),
        "apt-get update reported a failure (live third party: deb.debian.org, security.debian.org): {out}"
    );
    wait_for_connection_record(&vm, "deb.debian.org", 80, before).await;
    eprintln!("open rows: {:?}", open_rows(&vm, &workspace));

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Python's own clients with an empty environment, so neither `urllib` nor `pip` sees a proxy
/// setting or a certificate variable: `urllib` reaches an allowed name, is refused at once for an
/// unlisted one and that makes an inbox row, and `pip download` fetches a package from an index
/// a rule lists. The image ships no `pip`, so the test installs Debian's `python3-pip` first
/// (the live package mirrors, named when they fail).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_capture_python_urllib_and_pip_work_with_an_empty_environment() {
    let kit = PackageKit::open(
        Flavour::Apt,
        puddle_api::DEFAULT_IMAGE,
        "python3 python3-pip",
        "deb.debian.org and security.debian.org",
    );
    let net = web_internet(
        &[SITE, INDEX, KIT_HOST],
        kit.options(WebOptions {
            responder: Some(pip_index(INDEX, PROJECT)),
            ..WebOptions::default()
        }),
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("pyth", net.options, &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace(
        &vm,
        &api,
        "pyth",
        &[
            SITE,
            INDEX,
            KIT_HOST,
            "deb.debian.org",
            "security.debian.org",
        ],
    )
    .await;
    let run = |script: String| {
        let (runtime, workspace) = (&vm.runtime, &workspace);
        async move { guest(runtime, workspace, &bash_script(&script)).await }
    };

    let installed = kit
        .install(&vm, &workspace, "python3 -m pip --version")
        .await;
    assert!(
        installed.contains("pip "),
        "python3-pip is not usable: {installed}"
    );

    // An allowed name through `urllib`: it resolves to a stand-in, the capture redirects the
    // connection, and the fake website sees the request.
    let before = allowed_connections(&vm, SITE, 443);
    let allowed = run(format!(
        "env -i PATH=/usr/bin:/bin python3 -c \"
import urllib.request
r = urllib.request.urlopen('https://{SITE}/', timeout=30)
print('status=%d' % r.status)\"  2>&1; echo \"exit=$?\""
    ))
    .await;
    eprintln!("{allowed}");
    assert_eq!(field(&allowed, "status="), "200", "{allowed}");
    assert_eq!(field(&allowed, "exit="), "0", "{allowed}");
    wait_for_connection_record(&vm, SITE, 443, before).await;
    assert!(
        reached_by_host(&net.web, SITE) > 0,
        "the site did not see the capture's request: {:?}",
        net.web.seen()
    );

    // A name nothing allows: the same call raises at once, and the connect is what asks.
    let refused = run("s=$(now); env -i PATH=/usr/bin:/bin python3 -c \"
import urllib.request
urllib.request.urlopen('https://unlisted.invalid/', timeout=20)\" 2>&1 | tail -3
echo \"exit=${PIPESTATUS[0]} ms=$(($(now) - s))\""
        .into())
    .await;
    eprintln!("{refused}");
    assert_ne!(field(&refused, "exit="), "0", "{refused}");
    assert!(
        field(&refused, "ms=").parse::<u64>().unwrap() < 5000,
        "{refused}"
    );
    wait_for_row(&vm, &workspace, "unlisted.invalid").await;
    assert_eq!(
        reached_by_host(&net.web, "unlisted.invalid"),
        0,
        "an unlisted name reached the site"
    );

    // `pip download` from the listed index, with only a flag for where the index is.
    let before = allowed_connections(&vm, INDEX, 443);
    let pip = run(format!(
        "rm -rf /tmp/pipd
         env -i PATH=/usr/bin:/bin HOME=/root python3 -m pip download --no-deps --retries 0 --timeout 20 \
           -d /tmp/pipd --index-url https://{INDEX}/simple {PROJECT} 2>&1 | tail -5
         echo \"pip=${{PIPESTATUS[0]}}\"
         ls /tmp/pipd"
    ))
    .await;
    eprintln!("{pip}");
    assert_eq!(field(&pip, "pip="), "0", "{pip}");
    assert!(pip.contains("puddle_probe-1.0.0-py3-none-any.whl"), "{pip}");
    wait_for_connection_record(&vm, INDEX, 443, before).await;
    assert!(
        net.web
            .seen()
            .iter()
            .any(|r| r.target.starts_with("/files/puddle_probe-1.0.0")),
        "the index never served the wheel: {:?}",
        net.web.seen()
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The fake sites of the Docker test: one name per phase and bridge, so each approval is its own.
const DOCKER_SITES: [&str; 4] = [
    "boot-default.invalid",
    "boot-user.invalid",
    "restart-default.invalid",
    "restart-user.invalid",
];

/// What the guest says about the agent, dockerd and the firewall tables: the start tick of the
/// oldest `puddle-agent` and of `dockerd` (the kernel's clock ticks since boot, field 22 of
/// `/proc/<pid>/stat`), and the `table` lines of `nft -a list ruleset` (with handles).
async fn firewall_state(vm: &VmHost, workspace: &WorkspaceId) -> String {
    let (code, out) = guest_as(
        &vm.runtime,
        workspace,
        "root",
        r#"first() { for s in /proc/[0-9]*/stat; do
    read -r pid comm _ 2>/dev/null <"$s" && [ "$comm" = "($1)" ] && awk '{print $22}' "$s"
done | sort -n | head -1; }
echo "agent_start=$(first puddle-agent)"
echo "dockerd_start=$(first dockerd)"
nft -a list ruleset | grep '^table '"#,
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(code, 0, "{out}");
    out
}

/// A container's fetch of `site` that no rule allows yet: it fails at once and makes an inbox row.
/// Then the row is approved and the same fetch works, with no restart. The audit names the site.
async fn refused_then_approved(
    vm: &VmHost,
    net: &WebInternet,
    workspace: &WorkspaceId,
    (docker, network): (&str, &str),
    site: &str,
) {
    let fetch = container_fetch(docker, network, &format!("http://{site}/"));
    let before = allowed_connections(vm, site, 80);
    let refused = as_agent(vm, workspace, &fetch).await.1;
    eprintln!("{network} to {site}, no rule: {refused}");
    assert!(
        !refused.to_lowercase().contains("proxy"),
        "the container had a proxy variable: {refused}"
    );
    assert_ne!(field(&refused, "exit="), "0", "{refused}");
    assert!(
        field(&refused, "ms=").parse::<u64>().unwrap() < 8000,
        "{refused}"
    );
    let id = wait_for_row(vm, workspace, site).await;
    vm.host
        .store()
        .resolve_pending(id, &Resolution::allow(), Actor::Cli)
        .unwrap();
    let approved = as_agent(vm, workspace, &fetch).await.1;
    eprintln!("{network} to {site}, approved: {approved}");
    assert_eq!(field(&approved, "exit="), "0", "{approved}");
    wait_for_connection_record(vm, site, 80, before).await;
    assert!(
        reached_by_host(&net.web, site) > 0,
        "the site did not see the request: {:?}",
        net.web.seen()
    );
}

/// The container checks of one phase, as `agent`: the ruleset holds the capture table and
/// Docker's tables, a container on the default bridge and one on a user bridge, each with no proxy
/// variable, reach the fake site named `{phase}-default.invalid` and `{phase}-user.invalid` only
/// through a rule (refused at once, then approved and retried with no restart), traffic between two
/// containers on the user bridge is not redirected, and the audit names the host.
async fn docker_phase(vm: &VmHost, net: &WebInternet, workspace: &WorkspaceId, phase: &str) {
    // dockerd answers (the image's entrypoint starts it after the boot), and the image is built
    // in the guest from files the guest has, `FROM scratch`.
    eventually_async("dockerd to answer", Duration::from_secs(100), || async {
        let (code, out) = as_agent(
            vm,
            workspace,
            "id; ls -l /usr/bin/sudo /usr/local/bin/docker /usr/sbin/nft /bin/busybox; dsudo info 2>&1 >/dev/null | tail -3; \
             dsudo info >/dev/null 2>&1",
        )
        .await;
        (code == 0).then_some(()).ok_or(out)
    })
    .await;
    let (code, built) = as_agent(
        vm,
        workspace,
        &format!("dsudo image inspect probe:local >/dev/null 2>&1 || {{ {PROBE_IMAGE}; }}"),
    )
    .await;
    assert_eq!(code, 0, "{phase}: building the probe image: {built}");

    // Both firewalls are there at once (read as root: the image has no `sudo`).
    let (code, ruleset) = guest_as(
        &vm.runtime,
        workspace,
        "root",
        "nft list ruleset",
        Duration::from_secs(60),
    )
    .await;
    eprintln!("{phase}: ruleset\n{ruleset}");
    assert_eq!(code, 0, "{phase}: {ruleset}");
    assert!(ruleset.contains("table inet puddle"), "{phase}: {ruleset}");
    assert!(
        ruleset.contains("chain DOCKER") || ruleset.contains("docker-bridges"),
        "{phase}: Docker's tables are missing: {ruleset}"
    );
    let (_, groups) = as_agent(vm, workspace, "id -nG").await;
    eprintln!("{phase}: agent's groups: {groups}");
    let docker = "dsudo";

    // A container on the default bridge.
    refused_then_approved(
        vm,
        net,
        workspace,
        ("dsudo", "bridge"),
        &format!("{phase}-default.invalid"),
    )
    .await;

    // A user bridge: two containers talk to each other, and one reaches a site through a rule.
    let (bridge, server) = (format!("net-{phase}"), format!("srv-{phase}"));
    let (code, said) = as_agent(
        vm,
        workspace,
        &format!(
            "{docker} network create {bridge} >/dev/null && {docker} run -d --name {server} \
             --pull=never --network {bridge} probe:local /bin/busybox sh -c 'while true; do \
             printf \"HTTP/1.0 200 OK\\r\\n\\r\\ninner\\n\" | /bin/busybox nc -l -p 8080; done'"
        ),
    )
    .await;
    assert_eq!(code, 0, "{phase}: {said}");
    let talk = container_fetch(docker, &bridge, &format!("http://{server}:8080/"));
    let talked = eventually_async(
        "a container to reach its neighbour on the user bridge",
        Duration::from_secs(30),
        || async {
            let out = as_agent(vm, workspace, &talk).await.1;
            if field(&out, "exit=") == "0" {
                return Ok(out);
            }
            let said = as_agent(
                vm,
                workspace,
                &format!("{docker} ps -a; {docker} logs {server} 2>&1 | tail -5"),
            )
            .await
            .1;
            Err(format!("{out}\n{said}"))
        },
    )
    .await;
    eprintln!("{phase}: container to container: {talked}");
    assert!(talked.contains("inner"), "{talked}");
    refused_then_approved(
        vm,
        net,
        workspace,
        (docker, &bridge),
        &format!("{phase}-user.invalid"),
    )
    .await;

    // The neighbour's traffic never reached the proxy: no row and no audit line names it.
    assert!(
        !open_rows(vm, workspace).iter().any(|r| r.contains(&server)),
        "{:?}",
        open_rows(vm, workspace)
    );
    let audit = vm.host.store().audit_lines(0, 100_000).unwrap();
    assert!(
        !audit.iter().any(|(_, line)| line.contains(&server)),
        "the audit names the neighbour container"
    );
}

/// Docker's firewall tables and the agent's `inet puddle` table coexist in both orders, with a real
/// `dockerd` in the `docker:dind` image: dockerd started after the agent (the boot order), and the
/// agent restarted after dockerd was up. In both, containers on the default bridge and on a user
/// bridge get to a site only through a rule, and the session user works through `sudo`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_capture_coexists_with_a_real_dockerd_in_both_start_orders() {
    let kit = PackageKit::open(
        Flavour::Apk,
        "docker:dind",
        "nftables",
        "dl-cdn.alpinelinux.org",
    );
    let mut sites = DOCKER_SITES.to_vec();
    sites.push(KIT_HOST);
    let net = web_internet(
        &sites,
        kit.options(WebOptions::default()),
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("dock", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace_from(
        &vm,
        &api,
        "dock",
        &["dl-cdn.alpinelinux.org", KIT_HOST],
        json!({"image": "docker:dind", "memory_mib": 2048}),
    )
    .await;
    // The image ships no `nft`; the test brings one, through the proxy, as any tool would: from
    // the saved package files on a warm cache, from Alpine's mirror on the first run.
    let installed = kit.install(&vm, &workspace, "nft --version").await;
    assert!(
        installed.contains("nftables"),
        "nftables is not usable: {installed}"
    );

    // Boot order: the agent first, dockerd after it.
    docker_phase(&vm, &net, &workspace, "boot").await;
    let first = firewall_state(&vm, &workspace).await;
    eprintln!("{first}");
    let started = |text: &str, key: &str| field(text, key).parse::<u64>().unwrap();
    let agent = started(&first, "agent_start=");
    assert!(
        agent < started(&first, "dockerd_start="),
        "dockerd did not start after the agent: {first}"
    );

    // Kill the agent; the supervisor starts it again, with dockerd up.
    let (code, said) = guest_as(
        &vm.runtime,
        &workspace,
        "root",
        r#"for s in /proc/[0-9]*/stat; do
    read -r pid comm _ 2>/dev/null <"$s" && [ "$comm" = "(puddle-agent)" ] && agent="${agent:-} $pid"
done
kill -9 ${agent:?no puddle-agent process}"#,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(code, 0, "{said}");
    let second = eventually_async(
        "the agent to be back with its table written again",
        Duration::from_secs(60),
        || async {
            let state = firewall_state(&vm, &workspace).await;
            let back = field(&state, "agent_start=")
                .parse::<u64>()
                .is_ok_and(|t| t > agent)
                && state.contains("table inet puddle");
            back.then_some(state)
        },
    )
    .await;
    eprintln!("{second}");
    // Docker's own tables were not touched (same handles); the capture table is a new one.
    let tables = |text: &str, ours: bool| -> Vec<String> {
        text.lines()
            .filter(|l| l.starts_with("table ") && l.contains("table inet puddle") == ours)
            .map(str::to_owned)
            .collect()
    };
    assert_eq!(
        tables(&first, false),
        tables(&second, false),
        "the agent changed Docker's tables"
    );
    assert_ne!(
        tables(&first, true),
        tables(&second, true),
        "the agent did not write its table again"
    );
    docker_phase(&vm, &net, &workspace, "restart").await;

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
