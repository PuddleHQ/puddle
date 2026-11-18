// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rules as a tool in a real guest sees them (K on Linux KVM, W on Windows WHP): two bare
//! workspaces, and the session user's `curl` with the environment a terminal gets, which reads
//! the status, the body and the `x-puddle-*` headers of every answer. Each step decides through
//! the API the way the Inbox and the Rules page do, and then probes both workspaces:
//!
//! - an unknown host is `403` pending with the text that says where to approve it;
//! - an approval for one workspace opens it there only; deleting the rule closes it again; an
//!   approval for every workspace opens it in both;
//! - a suffix approval covers the names below the domain and never a look-alike that only
//!   contains it;
//! - a deny is `403` with the rule's number in a header and in the text, in its workspace only;
//! - a workspace's exact deny beats a global suffix allow (R-6);
//! - a rule on method and path lets a `GET` through and refuses a `POST` naming the rule;
//! - a listener on the host's loopback is refused while its toggle is off (no row), asks once
//!   it is on, is reached after an approval, and is refused again when the toggle goes off,
//!   whatever the rule says;
//! - a raw TCP connection to port 22 of an unknown name makes a row for `<host>:22` at once.
//!
//! The sites are a fake website behind a fake company proxy (`web_internet`), so no result here
//! depends on a public site. The `nc` the last step needs is Debian's, saved from the real mirror
//! on the first run and served from the fake website after that (`vm_support::PackageKit`).
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test, a missing JSON key included; what the guest said goes to the log"
)]

mod support;
mod vm_support;

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use puddle_e2e::lab::WebOptions;
use puddle_test_kit::eventually;
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, named_id_options};
use vm_support::kit::{Flavour, KIT_HOST, PackageKit};
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host_platform, web_internet};

/// The host every step starts from: nothing allows it.
const ASKED: &str = "asked.invalid";
/// The domain of the suffix approval, a name below it, and a look-alike that only contains it.
const SUFFIX: &str = "sfx.invalid";
const BELOW: &str = "www.sfx.invalid";
const LOOK_ALIKE: &str = "x.sfx.invalid.evil.invalid";
/// The host one workspace denies.
const DENIED: &str = "denied.invalid";
/// The domain of the global suffix allow, a name below it, and the name a workspace denies.
const WINS: &str = "wins.invalid";
const GOOD: &str = "good.wins.invalid";
const BAD: &str = "bad.wins.invalid";
/// The host with a rule on method and path.
const METHODS: &str = "methods.invalid";
/// The name a raw TCP connection to port 22 asks for.
const SSH_HOST: &str = "ssh.invalid";

/// What one `curl` in the guest saw.
#[derive(Debug, Default, Clone)]
struct Seen {
    /// The final status (the `CONNECT` answer's when the tunnel was refused).
    code: u16,
    /// Every `x-puddle-*` header, name lower-cased.
    headers: Vec<(String, String)>,
    /// The body, newlines folded into spaces.
    body: String,
    /// What curl printed on stderr.
    error: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// `<code> <decision>`, the playbook's short form (`403 pending`, `200`).
    fn short(&self) -> String {
        match self.header("x-puddle-decision") {
            Some(decision) => format!("{} {decision}", self.code),
            None => self.code.to_string(),
        }
    }
}

/// The shell function every probe script starts with: `probe <label> <curl args>` runs curl with
/// whatever proxy settings the login shell has and prints `code`, `header`, `body` and `error`
/// lines tagged with the label.
const PROBE_FN: &str = r#"probe() {
  l=$1; shift
  d=$(mktemp -d)
  w=$(curl -sS -o "$d/b" -D "$d/h" --max-time 20 -w '%{http_code} %{http_connect}' "$@" 2>"$d/e")
  echo "code $l $w"
  tr -d '\r' <"$d/h" | grep -i '^x-puddle-' | sed "s/^/header $l /"
  echo "body $l $(head -c 4000 "$d/b" | tr '\r\n' '  ')"
  echo "error $l $(tr '\r\n' '  ' <"$d/e")"
  rm -rf "$d"
}
"#;

/// The guest's view of each labelled probe in `out`.
fn parse(out: &str, label: &str) -> Seen {
    let mut seen = Seen::default();
    let tag = |kind: &str| format!("{kind} {label} ");
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix(&tag("code")) {
            // `<http_code> <http_connect>`: a refused CONNECT leaves the first at 000.
            let codes: Vec<u16> = rest
                .split_whitespace()
                .filter_map(|c| c.parse().ok())
                .collect();
            seen.code = codes.iter().copied().find(|&c| c != 0).unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix(&tag("header")) {
            if let Some((name, value)) = rest.split_once(':') {
                seen.headers
                    .push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
            }
        } else if let Some(rest) = line.strip_prefix(&tag("body")) {
            rest.trim().clone_into(&mut seen.body);
        } else if let Some(rest) = line.strip_prefix(&tag("error")) {
            rest.trim().clone_into(&mut seen.error);
        }
    }
    seen
}

/// One workspace of the battery: its id and its name in the API.
struct Guest {
    id: WorkspaceId,
    label: &'static str,
}

/// The battery's state: the host, its API, and every mismatch found so far, so one run names
/// every rule that misbehaves instead of the first.
struct Battery {
    vm: VmHost,
    api: Api,
    a: Guest,
    b: Guest,
    misses: std::cell::RefCell<Vec<String>>,
}

impl Battery {
    /// Runs `curl <args>` for each `(label, args)` in `workspace` as the session user, in a
    /// login shell (the environment a terminal gets), and returns what each saw.
    async fn probe(&self, workspace: &Guest, probes: &[(&str, String)]) -> Vec<Seen> {
        let mut script = String::from(PROBE_FN);
        for (label, args) in probes {
            writeln!(script, "probe {label} {args}").unwrap();
        }
        let (code, out) = guest_as(
            &self.vm.runtime,
            &workspace.id,
            "agent",
            &script,
            Duration::from_secs(120),
        )
        .await;
        eprintln!("--- {} ---\n{out}", workspace.label);
        assert_eq!(
            code, 0,
            "the probe script failed in {}: {out}",
            workspace.label
        );
        probes.iter().map(|(label, _)| parse(&out, label)).collect()
    }

    /// `http://<host>/` and `https://<host>/` from `workspace`: the plain one carries the body a
    /// tool shows, the TLS one the answer to the tunnel.
    async fn both(&self, workspace: &Guest, host: &str) -> (Seen, Seen) {
        let seen = self
            .probe(
                workspace,
                &[
                    ("plain", format!("http://{host}/")),
                    ("tls", format!("https://{host}/")),
                ],
            )
            .await;
        let mut seen = seen.into_iter();
        (seen.next().unwrap(), seen.next().unwrap())
    }

    /// Records a mismatch unless `ok`.
    fn check(&self, ok: bool, step: &str, what: &str, seen: &Seen) {
        if !ok {
            let miss = format!("{step}: {what}; the guest saw {seen:?}");
            eprintln!("MISMATCH {miss}");
            self.misses.borrow_mut().push(miss);
        }
    }

    /// Records a mismatch unless both answers to `host` from `workspace` are `wanted` (`200`,
    /// `403 pending`, `403 deny`).
    async fn expect(&self, step: &str, workspace: &Guest, host: &str, wanted: &str) -> Seen {
        let (plain, tls) = self.both(workspace, host).await;
        let what = format!("{host} from {} should be {wanted}", workspace.label);
        self.check(
            plain.short() == wanted,
            step,
            &format!("{what} (http)"),
            &plain,
        );
        self.check(
            tls.short() == wanted,
            step,
            &format!("{what} (https)"),
            &tls,
        );
        plain
    }

    /// The pending id a refusal named.
    fn pending_id(&self, step: &str, seen: &Seen) -> Option<i64> {
        let id = seen.header("x-puddle-pending").and_then(|v| v.parse().ok());
        self.check(id.is_some(), step, "an x-puddle-pending header", seen);
        id
    }

    /// Approves or denies pending request `id` through the API with `body` and returns the rule
    /// it made.
    async fn decide(&self, id: i64, verb: &str, body: serde_json::Value) -> serde_json::Value {
        let reply = self
            .api
            .post(&format!("/api/pending/{id}/{verb}"), &body.to_string())
            .await;
        assert_eq!(reply.status, 200, "{verb} {id}: {}", reply.body);
        reply.json()["rule"].clone()
    }

    /// Makes a rule through the API (the Rules page's "Add rule") and returns its id.
    async fn add_rule(&self, body: serde_json::Value) -> i64 {
        let reply = self.api.post("/api/rules", &body.to_string()).await;
        assert_eq!(reply.status, 201, "{body}: {}", reply.body);
        reply.json()["id"].as_i64().unwrap()
    }

    /// The open inbox rows of `workspace`, as `host:port`.
    fn open_rows(&self, workspace: &Guest) -> Vec<String> {
        self.vm
            .host
            .store()
            .open_pending_every_user(&puddle_store::testing::all_users(), Some(&workspace.id))
            .unwrap()
            .iter()
            .map(|r| format!("{}:{}", r.host, r.port))
            .collect()
    }

    /// Sets workspace A's "This computer" toggle.
    async fn loopback_toggle(&self, on: bool) {
        let reply = self
            .api
            .put(
                &format!("/api/settings/workspaces/{}", self.a.id),
                &json!({"overrides": {"local_toggles": {"loopback": on}}}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    }
}

/// A listener on the host's loopback that answers every request `200` with `loopback here`.
async fn loopback_listener() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let server = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = server.accept().await {
            tokio::spawn(async move {
                let mut buf = [0_u8; 4096];
                let _ = conn.read(&mut buf).await;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nConnection: close\r\n\r\nloopback here\n",
                    )
                    .await;
            });
        }
    });
    port
}

/// Every rule step of the firewall playbook, probed from a real guest with the tools a person
/// uses there: the status, the decision headers and the text each refusal gives.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_rules_probe_a_guest_tool_sees_every_rule_decide_its_answer() {
    // The image ships no `nc`; the test brings Debian's, through the proxy, as any tool would:
    // from the saved package files on a warm cache, from Debian's mirrors on the first run.
    let kit = PackageKit::open(
        Flavour::Apt,
        puddle_api::DEFAULT_IMAGE,
        "netcat-openbsd",
        "deb.debian.org and security.debian.org",
    );
    let sites = [ASKED, BELOW, DENIED, GOOD, BAD, METHODS, KIT_HOST];
    let net = web_internet(
        &sites,
        kit.options(WebOptions::default()),
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("rprb", net.options, &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let extra = || json!({"memory_mib": 512});
    let a = Guest {
        id: create_workspace_from(
            &vm,
            &api,
            "rpa",
            &[KIT_HOST, "deb.debian.org", "security.debian.org"],
            extra(),
        )
        .await,
        label: "A",
    };
    let installed = kit.install(&vm, &a.id, "command -v nc").await;
    assert!(
        installed.trim_end().ends_with("/nc"),
        "netcat is not usable: {installed}"
    );
    let b = Guest {
        id: create_workspace_from(&vm, &api, "rpb", &[], extra()).await,
        label: "B",
    };
    let t = Battery {
        vm,
        api,
        a,
        b,
        misses: std::cell::RefCell::default(),
    };

    // S1: nothing is allowed by default. The text says where to approve and that a retry follows.
    for w in [&t.a, &t.b] {
        let seen = t.expect("S1", w, ASKED, "403 pending").await;
        t.check(
            seen.body
                == format!(
                    "puddle: {ASKED} is waiting for approval in Puddle; approve it there, then retry"
                ),
            "S1",
            "the body is the waiting-for-approval text",
            &seen,
        );
    }

    // S2: approve for A only. A gets through, B still waits.
    let (asked_in_a, _) = t.both(&t.a, ASKED).await;
    let rule = match t.pending_id("S2", &asked_in_a) {
        Some(id) => t.decide(id, "approve", json!({})).await,
        None => json!(null),
    };
    t.expect("S2", &t.a, ASKED, "200").await;
    t.expect("S2", &t.b, ASKED, "403 pending").await;

    // S2 undo: deleting the rule closes it in A again.
    let rule_id = rule["id"].as_i64().unwrap_or_default();
    let reply = t.api.delete(&format!("/api/rules/{rule_id}"), None).await;
    assert!(
        reply.status == 200 || reply.status == 204,
        "deleting rule {rule_id}: {} {}",
        reply.status,
        reply.body
    );
    t.expect("S2 undo", &t.a, ASKED, "403 pending").await;

    // S3: approve for every workspace: both get through.
    let (asked_in_b, _) = t.both(&t.b, ASKED).await;
    if let Some(id) = t.pending_id("S3", &asked_in_b) {
        t.decide(id, "approve", json!({"scope": "global"})).await;
    }
    t.expect("S3", &t.a, ASKED, "200").await;
    t.expect("S3", &t.b, ASKED, "200").await;

    // S4: a suffix approval covers the name below it, never a look-alike that only contains it.
    let (below, _) = t.both(&t.a, BELOW).await;
    if let Some(id) = t.pending_id("S4", &below) {
        t.decide(id, "approve", json!({"suffix": SUFFIX})).await;
    }
    t.expect("S4", &t.a, BELOW, "200").await;
    t.expect("S4", &t.a, LOOK_ALIKE, "403 pending").await;

    // S6: deny in B. The refusal names the rule in a header and in the text; A still waits.
    let (denied_in_b, _) = t.both(&t.b, DENIED).await;
    let deny = match t.pending_id("S6", &denied_in_b) {
        Some(id) => t.decide(id, "deny", json!({})).await,
        None => json!(null),
    };
    let deny_id = deny["id"].as_i64().unwrap_or_default().to_string();
    let (plain, tls) = t.both(&t.b, DENIED).await;
    for seen in [&plain, &tls] {
        t.check(seen.short() == "403 deny", "S6", "a 403 deny", seen);
        t.check(
            seen.header("x-puddle-rule") == Some(deny_id.as_str()),
            "S6",
            &format!("x-puddle-rule: {deny_id}"),
            seen,
        );
    }
    t.check(
        plain.body == format!("puddle: {DENIED} is denied by a rule (rule {deny_id})"),
        "S6",
        &format!("the body names rule {deny_id}"),
        &plain,
    );
    t.expect("S6", &t.a, DENIED, "403 pending").await;

    // S7: a global suffix allow and a workspace's exact deny: the deny wins in its workspace
    // (R-6), the allow everywhere else.
    t.add_rule(
        json!({"scope": {"type": "global"}, "pattern": format!("*.{WINS}"), "effect": "allow"}),
    )
    .await;
    let bad_id = t
        .add_rule(json!({
            "scope": {"type": "workspace", "workspace": t.a.id.as_str()},
            "pattern": BAD,
            "effect": "deny",
        }))
        .await;
    t.expect("S7", &t.a, GOOD, "200").await;
    let seen = t.expect("S7", &t.a, BAD, "403 deny").await;
    t.check(
        seen.header("x-puddle-rule") == Some(bad_id.to_string().as_str()),
        "S7",
        &format!("x-puddle-rule: {bad_id}"),
        &seen,
    );
    t.expect("S7", &t.b, BAD, "200").await;

    // S12: a rule on method and path. GET goes through, POST is refused naming the rule, and
    // the site never sees the POST.
    t.add_rule(json!({
        "scope": {"type": "workspace", "workspace": t.a.id.as_str()},
        "pattern": METHODS,
        "effect": "allow",
    }))
    .await;
    let reply = t
        .api
        .post(
            "/api/request-rules",
            &json!({"workspace": t.a.id.as_str(), "host": METHODS, "methods": ["POST"], "path": "/anything"})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let request_rule = reply.json()["id"].as_i64().unwrap().to_string();
    let url = format!("https://{METHODS}/anything");
    let seen = t
        .probe(
            &t.a,
            &[
                ("get", url.clone()),
                ("post", format!("-X POST -d probe=1 {url}")),
            ],
        )
        .await;
    let [get, post] = seen.as_slice() else {
        panic!("two probes, two answers: {seen:?}")
    };
    t.check(get.short() == "200", "S12", "GET is 200", get);
    t.check(post.short() == "403 deny", "S12", "POST is 403 deny", post);
    t.check(
        post.header("x-puddle-request-rule") == Some(request_rule.as_str()),
        "S12",
        &format!("x-puddle-request-rule: {request_rule}"),
        post,
    );
    t.check(
        post.body
            .contains(&format!("(request rule {request_rule})")),
        "S12",
        &format!("the body names request rule {request_rule}"),
        post,
    );
    let posted = net
        .web
        .seen()
        .iter()
        .filter(|r| r.method == "POST" && r.target.starts_with("/anything"))
        .count();
    t.check(posted == 0, "S12", "the site never saw the POST", post);

    // S13: a listener on the host's loopback. Off: refused naming the toggle, no row. On: it
    // asks. Approved: reached. Off again: refused whatever the rule says (R-14).
    let listener = loopback_listener().await;
    let target = format!("127.0.0.1:{listener}");
    // `--noproxy ''` clears the guest's NO_PROXY (which keeps 127.0.0.1 inside the guest) and
    // keeps its proxy: the way a tool asks Puddle for this computer.
    let loopback = format!("--noproxy '' http://{target}/");
    let off = t.probe(&t.a, &[("loop", loopback.clone())]).await.remove(0);
    t.check(off.short() == "403 blocked", "S13 off", "403 blocked", &off);
    t.check(
        off.header("x-puddle-blocked") == Some("toggle:loopback"),
        "S13 off",
        "x-puddle-blocked: toggle:loopback",
        &off,
    );
    t.check(
        off.body.contains("turn on 'loopback'"),
        "S13 off",
        "the text names the toggle",
        &off,
    );
    let rows = t.open_rows(&t.a);
    t.check(
        !rows.iter().any(|r| r.starts_with("127.0.0.1:")),
        "S13 off",
        &format!("no inbox row for this computer, rows {rows:?}"),
        &off,
    );
    t.loopback_toggle(true).await;
    let on = t.probe(&t.a, &[("loop", loopback.clone())]).await.remove(0);
    t.check(on.short() == "403 pending", "S13 on", "403 pending", &on);
    if let Some(id) = t.pending_id("S13 on", &on) {
        t.decide(id, "approve", json!({})).await;
    }
    let allowed = t.probe(&t.a, &[("loop", loopback.clone())]).await.remove(0);
    t.check(allowed.short() == "200", "S13 allowed", "200", &allowed);
    t.check(
        allowed.body.contains("loopback here"),
        "S13 allowed",
        "the listener's answer",
        &allowed,
    );
    t.loopback_toggle(false).await;
    let off_again = t.probe(&t.a, &[("loop", loopback.clone())]).await.remove(0);
    t.check(
        off_again.short() == "403 blocked"
            && off_again.header("x-puddle-blocked") == Some("toggle:loopback"),
        "S13 off again",
        "403 blocked by the toggle, the rule notwithstanding",
        &off_again,
    );

    // S14b: raw TCP to port 22 of an unknown name: nc is refused and a row for `<host>:22` is
    // open within five seconds.
    let started = Instant::now();
    let (_, said) = guest_as(
        &t.vm.runtime,
        &t.a.id,
        "agent",
        &format!("nc -vz -w 5 {SSH_HOST} 22; echo \"exit=$?\""),
        Duration::from_secs(60),
    )
    .await;
    eprintln!("--- A nc ---\n{said}");
    let wanted = format!("{SSH_HOST}:22");
    eventually(
        &format!("an open row for {wanted}"),
        Duration::from_secs(10),
        || {
            let rows = t.open_rows(&t.a);
            rows.contains(&wanted)
                .then_some(())
                .ok_or_else(|| format!("{rows:?}"))
        },
    )
    .await;
    let took = started.elapsed();
    let nc = Seen {
        error: said.clone(),
        ..Seen::default()
    };
    t.check(
        took < Duration::from_secs(5),
        "S14b",
        &format!("the row {wanted} within 5 s, took {took:?}"),
        &nc,
    );

    let Battery { vm, misses, .. } = t;
    let misses = misses.into_inner();
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
    assert!(
        misses.is_empty(),
        "{} mismatches:\n{}",
        misses.len(),
        misses.join("\n")
    );
}
