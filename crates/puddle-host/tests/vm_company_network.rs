// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company-network category on a real microVM (K on Linux KVM, W on Windows WHP): a guest
//! with no network of its own asks for a name that only a private-access client resolves.
//!
//! The host reads a private-access client's signals ([`puddle_e2e::lab::PrivateAccess`]: the
//! client's adapter, its DNS suffix and its route, all as data; nothing on the machine changes),
//! resolves the intranet name to an address in `100.64.0.0/10`, and reaches the intranet site by
//! name through the client's tunnel.
//!
//! - `vm_company_network_*`: the guest's own `curl` is refused with `403` and
//!   `x-puddle-blocked: company_network` while the category's toggle is off, also when an exact
//!   allow rule exists and the private toggle is on; with the toggle on the same request reaches
//!   the site, and the audit says which category it is.
//! - `vm_radar_company_network_*`: the real tools of the corporate-network acceptance battery
//!   (`curl`, `git`, `npm`, with a 3 MB download from the intranet) run once with the toggle off
//!   and once with it on, so the toggle changes the intranet's answer and nothing else. The tools
//!   come from live package mirrors and the public internet, so this one is named `vm_radar_*`:
//!   the per-pull-request jobs skip it.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test"
)]

mod support;
mod vm_support;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use puddle_e2e::lab::{PrivateAccess, WebAnswer, WebOptions};
use puddle_host::HostOptions;
use puddle_netpolicy::StaticSignals;
use puddle_proxy::{BoxFuture, Resolver, SystemResolver};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::{DomainName, WorkspaceId};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{Api, ended, named_id_options};
use vm_support::{
    VmHost, guest_within, platform_trusting, start_vm_host_platform, through_company_proxy_port,
};

const BLOB: &str = "/blob.bin";
const BLOB_LEN: usize = 3 * 1024 * 1024;

/// The host's resolver: the intranet name answers inside `100.64.0.0/10`, as the client's DNS
/// does, and every other name goes to the machine's own.
struct ClientDns;

impl Resolver for ClientDns {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        for (known, ip) in PrivateAccess::answers() {
            if known.eq_ignore_ascii_case(name.as_str()) {
                return Box::pin(async move { Ok(vec![SocketAddr::new(ip, port)]) });
            }
        }
        SystemResolver.resolve(name, port)
    }
}

/// The 3 MB the intranet serves at [`BLOB`]: bytes that repeat, so a short read is not mistaken
/// for the whole.
fn blob() -> Vec<u8> {
    (0..BLOB_LEN)
        .map(|i| u8::try_from((i * 7 + i / 251) % 256).unwrap())
        .collect()
}

fn blob_sha256() -> String {
    use std::fmt::Write as _;
    Sha256::digest(blob())
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// What a host with the private-access client reads of its network.
fn signals() -> StaticSignals {
    let adapter = PrivateAccess::adapter();
    StaticSignals::new()
        .with_private_access_client()
        .with_dns_suffix(adapter.dns_suffix)
        .with_vpn_route(IpAddr::V4(adapter.route.0), adapter.route.1)
}

/// The intranet behind the client, a host that reads the client's signals, and a started
/// workspace that may reach the intranet name (an exact allow rule).
struct Lab {
    access: PrivateAccess,
    vm: VmHost,
    api: Api,
    workspace: WorkspaceId,
}

async fn boot(tag: &str, extra_allows: &[&str], memory_mib: u32) -> Lab {
    let access = PrivateAccess::start_with(WebOptions {
        responder: Some(Arc::new(|seen| {
            (seen.target == BLOB).then(|| WebAnswer {
                status: 200,
                content_type: "application/octet-stream",
                body: blob(),
            })
        })),
        ..WebOptions::default()
    })
    .await;
    let mut options: HostOptions =
        through_company_proxy_port(named_id_options(), access.tunnel_port());
    options.resolver = Some(Arc::new(ClientDns));
    options.company_signals = Some(Arc::new(signals()));
    let platform = platform_trusting([access.root()]);
    let vm = start_vm_host_platform(tag, options, &platform, |_| {}).await;
    let name = format!("{}-{tag}", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    for site in std::iter::once(&PrivateAccess::NAME).chain(extra_allows) {
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
            &json!({"name": name, "memory_mib": memory_mib}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let name_static: &'static str = Box::leak(name.into_boxed_str());
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    Lab {
        access,
        vm,
        api,
        workspace,
    }
}

impl Lab {
    /// Sets the workspace's toggles (`company_network`, `private`) to the given values.
    async fn toggles(&self, company_network: bool, private: bool) {
        let reply = self
            .api
            .put(
                &format!("/api/settings/workspaces/{}", self.workspace),
                &json!({"overrides": {"local_toggles": {"company_network": company_network, "private": private}}})
                    .to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    }

    async fn guest(&self, script: &str, limit: Duration) -> String {
        guest_within(&self.vm.runtime, &self.workspace, script, limit).await
    }

    /// The connection records of the workspace to the intranet name, newest first.
    async fn records(&self) -> Vec<Value> {
        let reply = self
            .api
            .get(&format!(
                "/api/audit?workspace={}&type=connection&limit=200",
                self.workspace
            ))
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let body: Value = serde_json::from_str(&reply.body).unwrap();
        body["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["record"].clone())
            .filter(|r| r["host"] == PrivateAccess::NAME)
            .collect()
    }

    async fn stop(self) {
        let down = self.vm.host.shutdown().await;
        assert!(down.sandboxes.all_stopped(), "{down:?}");
    }
}

/// `curl` to the intranet name over plain HTTP: the status line, and the refusal header.
const PLAIN: &str = "curl -s -i --max-time 30 http://intranet.corp.test/ | tr -d '\\r' \
     | awk 'NR==1{s=$2} tolower($1)==\"x-puddle-blocked:\"{r=$2} END{print \"status=\" s \" reason=\" r}'";

/// `curl` to the intranet name over HTTPS: the proxy's answer to the tunnel request, or the
/// site's. Prints `status=<code>` from curl's own verbose output and the refusal header.
const TLS: &str = "o=$(curl -sv --max-time 30 -o /dev/null -w 'final=%{http_code}' https://intranet.corp.test/ 2>&1 | tr -d '\\r'); \
     echo \"$o\" | grep -i 'x-puddle-blocked' | head -1; echo \"$o\" | grep -E 'HTTP/1.1 [0-9]+|final=' | tr '\\n' ' '; echo";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_company_network_toggle_off_refuses_the_intranet_name_and_toggle_on_reaches_it() {
    let lab = boot("cnet", &[], 512).await;
    let limit = Duration::from_secs(120);

    // Toggle off: refused over both schemes, with an exact allow rule and the private toggle on.
    lab.toggles(false, true).await;
    let plain = lab.guest(PLAIN, limit).await;
    assert!(
        plain.contains("status=403") && plain.contains("reason=company_network"),
        "plain HTTP with the toggle off: {plain}"
    );
    let tls = lab.guest(TLS, limit).await;
    assert!(
        tls.to_ascii_lowercase()
            .contains("x-puddle-blocked: company_network")
            && tls.contains("403"),
        "HTTPS with the toggle off: {tls}"
    );
    assert!(
        lab.access.carried().is_empty() && lab.access.served().is_empty(),
        "the refused requests reached the client's tunnel or the site: {:?}",
        lab.access.carried()
    );
    let refused = lab.records().await;
    assert!(
        !refused.is_empty()
            && refused.iter().all(|r| r["decision"] == "deny"
                && r["category"] == "company_network"
                && r["signal"].is_string()),
        "the audit names the category: {refused:#?}"
    );

    // Toggle on (the private toggle off, so the company toggle alone decides): the same request
    // reaches the site by name through the client's tunnel.
    lab.toggles(true, false).await;
    let plain = lab.guest(PLAIN, limit).await;
    assert!(
        plain.contains("status=200"),
        "plain HTTP with the toggle on: {plain}"
    );
    let tls = lab.guest(TLS, limit).await;
    assert!(tls.contains("final=200"), "HTTPS with the toggle on: {tls}");
    let carried = lab.access.carried();
    assert!(
        carried.iter().any(|c| c == "intranet.corp.test:80")
            && carried.iter().any(|c| c == "intranet.corp.test:443"),
        "the tunnel carries the name, not the address: {carried:?}"
    );
    assert!(
        carried.iter().all(|c| !c.contains("100.64.")),
        "{carried:?}"
    );
    assert!(
        lab.access
            .served()
            .iter()
            .any(|s| s.headers_named("host") == [PrivateAccess::NAME]),
        "the site answered a request"
    );

    lab.stop().await;
}

/// Live hosts the battery reaches: the package mirrors and public sites of the real tools.
const PUBLIC: &[&str] = &[
    "github.com",
    "deb.debian.org",
    "security.debian.org",
    "registry.npmjs.org",
];

/// The battery's public part, in the guest: `RESULT,<phase>,<test>,PASS|FAIL,<detail>`.
fn public_battery(phase: &str) -> String {
    format!(
        r#"set -u; P={phase}; W=/tmp/battery-$P; rm -rf $W; mkdir -p $W
res() {{ if [ "$2" = 0 ]; then echo "RESULT,$P,$1,PASS,$3"; else echo "RESULT,$P,$1,FAIL,rc=$2 $3"; fi; }}
code=$(curl -sS -o /dev/null -w '%{{http_code}}' --max-time 60 https://github.com/ 2>$W/e1); rc=$?
[ "$rc" = 0 ] && [ "$code" != 200 ] && rc=1; res curl_https $rc "http=$code $(head -c 150 $W/e1 | tr '\n,' '  ')"
code=$(curl -sS -o /dev/null -w '%{{http_code}}' --max-time 60 http://deb.debian.org/debian/dists/stable/Release 2>$W/e2); rc=$?
[ "$rc" = 0 ] && [ "$code" != 200 ] && rc=1; res curl_plain_http $rc "http=$code $(head -c 150 $W/e2 | tr '\n,' '  ')"
git clone -q --depth 1 https://github.com/octocat/Hello-World.git $W/hw >$W/git.log 2>&1; rc=$?
[ "$rc" = 0 ] && [ ! -f $W/hw/README ] && rc=1; res git_clone $rc "$(tail -c 150 $W/git.log | tr '\n,' '  ')"
mkdir -p $W/npm && cd $W/npm && echo '{{"name":"battery","version":"1.0.0","private":true}}' > package.json
npm install is-odd --no-audit --no-fund --cache $W/npm-cache >$W/npm.log 2>&1; rc=$?
[ "$rc" = 0 ] && ! node -e "require('is-odd')" 2>>$W/npm.log && rc=1
res npm_install $rc "$(grep -E 'ERR|added' $W/npm.log | tail -2 | tr '\n,' '  ' | head -c 150)"
"#
    )
}

/// The battery's intranet part: the small page and the 3 MB download, as `RESULT` lines too.
fn intranet_battery(phase: &str, want_sha: &str) -> String {
    format!(
        r#"set -u; P={phase}; W=/tmp/battery-$P-intranet; rm -rf $W; mkdir -p $W
res() {{ echo "RESULT,$P,$1,$2,$3"; }}
code=$(curl -s -o $W/page -w '%{{http_code}}' --max-time 60 http://intranet.corp.test/ 2>$W/e1); res intranet_page "http=$code" ""
code=$(curl -s -o $W/blob -w '%{{http_code}}' --max-time 120 http://intranet.corp.test{BLOB} 2>$W/e2)
sum=$(sha256sum $W/blob 2>/dev/null | cut -d' ' -f1)
[ "$sum" = {want_sha} ] && same=yes || same=no
res intranet_3mb "http=$code" "sha_ok=$same"
"#
    )
}

fn results(out: &str) -> Vec<Vec<String>> {
    out.lines()
        .filter_map(|l| l.strip_prefix("RESULT,"))
        .map(|l| l.splitn(4, ',').map(str::to_owned).collect())
        .collect()
}

/// The tests of `rows` that passed, as a set of names.
fn passed(rows: &[Vec<String>]) -> Vec<&str> {
    rows.iter()
        .filter(|r| r.get(2).is_some_and(|s| s == "PASS"))
        .map(|r| r[1].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_company_network_toggle_changes_only_the_intranets_answer_for_the_real_tools() {
    let lab = boot("cnbat", PUBLIC, 3072).await;
    let long = Duration::from_mins(25);
    let install = lab
        .guest(
            "(apt-get update -qq && apt-get install -y -qq --no-install-recommends ca-certificates curl git nodejs npm) >/tmp/apt.log 2>&1; echo apt=$?; tail -3 /tmp/apt.log",
            long,
        )
        .await;
    assert!(
        install.contains("apt=0"),
        "the tools could not be installed (live third party: the Debian mirrors; an outage there fails this test): {install}"
    );
    let want = blob_sha256();
    let wanted_public = ["curl_https", "curl_plain_http", "git_clone", "npm_install"];

    // Toggle off: every public part passes; the intranet is refused and its bytes never leave.
    lab.toggles(false, false).await;
    let off_public = lab.guest(&public_battery("off"), long).await;
    let rows = results(&off_public);
    let ok = passed(&rows);
    for t in wanted_public {
        assert!(
            ok.contains(&t),
            "toggle off, {t} failed (live third parties): {off_public}"
        );
    }
    let off_intranet = lab.guest(&intranet_battery("off", &want), long).await;
    assert!(
        off_intranet.contains("RESULT,off,intranet_page,http=403")
            && off_intranet.contains("RESULT,off,intranet_3mb,http=403,sha_ok=no"),
        "toggle off, the intranet must be refused: {off_intranet}"
    );
    assert!(
        lab.access.served().is_empty(),
        "the site was reached with the toggle off"
    );

    // Toggle on: the same public parts still pass, and the intranet answers with its bytes.
    lab.toggles(true, false).await;
    let on_public = lab.guest(&public_battery("on"), long).await;
    let rows = results(&on_public);
    let ok = passed(&rows);
    for t in wanted_public {
        assert!(
            ok.contains(&t),
            "toggle on, {t} failed (live third parties): {on_public}"
        );
    }
    let on_intranet = lab.guest(&intranet_battery("on", &want), long).await;
    assert!(
        on_intranet.contains("RESULT,on,intranet_page,http=200")
            && on_intranet.contains("RESULT,on,intranet_3mb,http=200,sha_ok=yes"),
        "toggle on, the intranet must answer: {on_intranet}"
    );
    assert!(
        lab.access
            .served()
            .iter()
            .any(|s| s.target == BLOB && s.status == 200),
        "the site served the download"
    );

    lab.stop().await;
}
