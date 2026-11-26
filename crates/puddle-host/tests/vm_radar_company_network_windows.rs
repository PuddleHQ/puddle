// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company-network chain on Windows, end to end on a real microVM (WHP): a TLS-inspecting
//! proxy that also demands the Windows sign-in, as the machine's own proxy, with its root in the
//! current user's store.
//!
//! Nothing is faked on the host side: the host reads the roots from the real `CurrentUser\Root`
//! store and the proxy from the user's Internet Settings (what WinHTTP reports), and signs in
//! through SSPI as the logged-on user of the runner. The lab interceptor
//! ([`puddle_e2e::lab::Interceptor::start_signing_in_on_ports`]) answers `407` with an NTLM
//! challenge, accepts the Type 3, then terminates every tunnel with a root made for the run.
//!
//! - `vm_radar_company_network_windows_chain_*`: the root goes into `HKCU` Root and the
//!   interceptor into `ProxyServer`; `/api/network-health` lists the root and the proxy, a new
//!   workspace's guest `curl https://...` (no `-k`) gets the site's answer, the interceptor saw
//!   the three NTLM legs and the health report's attempt names NTLM or Negotiate as `signed_in`;
//!   the root is removed by its thumbprint at the end and the stores no longer hold it.
//! - `vm_radar_company_network_windows_without_the_root_*`: the same rig with the root sync
//!   skipped (nothing added to the store): the report lists no such root and the guest `curl`
//!   fails, so the chain test cannot pass by the guest trusting the interceptor some other way.
//!
//! Named `vm_radar_*` so the per-pull-request VM jobs skip it (it changes the runner user's
//! registry, which only `company-network-windows.yml` does on purpose); that workflow runs it.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test"
)]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use puddle_certs::{CorporateRoots, read_host_stores};
use puddle_e2e::lab::{Interceptor, NtlmLeg, WebAnswer, WebHost, WebOptions};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceId;
use puddle_upstream::Mode;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{Api, ended, named_id_options};
use vm_support::{HarnessPlatform, VmHost, guest_within, start_vm_host_platform};

/// The site behind the interceptor. `.invalid` never resolves anywhere: only the interceptor's
/// split DNS knows it.
// flake-ok: the name is answered by the interceptor's own table, the machine's resolver is never asked.
const SITE: &str = "chain.corp.invalid";
const PROBE: &str = "/chain-probe";
const ANSWER: &str = "windows-chain-ok";

const SETTINGS_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";

/// One test at a time may change the user's Internet Settings and Root store, in any process:
/// the same lock file `windows_pac.rs` of `puddle-upstream` holds.
#[expect(
    clippy::disallowed_methods,
    reason = "the lock is shared by every test process, so it must sit at one fixed path"
)]
fn exclusive() -> std::fs::File {
    let path = std::env::temp_dir().join("puddle-test-hkcu-internet-settings.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .unwrap();
    lock.lock().unwrap();
    lock
}

/// The gate for a test that edits the user's registry: `None` (skip) off a CI runner, otherwise
/// the exclusive lock. The workflow sets `PUDDLE_REQUIRE_REGISTRY_TESTS`, which makes a runner
/// that stops qualifying a failure.
fn registry_test() -> Option<std::fs::File> {
    if std::env::var_os("GITHUB_ACTIONS").is_none() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_REGISTRY_TESTS").is_none(),
            "PUDDLE_REQUIRE_REGISTRY_TESTS is set but this is not a CI runner (GITHUB_ACTIONS)"
        );
        eprintln!("skipped: edits the user's registry, only run on a CI runner (GITHUB_ACTIONS)");
        return None;
    }
    Some(exclusive())
}

fn reg(args: &[&str]) -> bool {
    Command::new("reg")
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

/// Puts one value in the Internet Settings key; puts a value that did not exist back to absent.
struct RegValue {
    name: &'static str,
    existed: bool,
}

impl RegValue {
    fn set(name: &'static str, kind: &str, data: &str) -> Self {
        let existed = reg(&["query", SETTINGS_KEY, "/v", name]);
        assert!(
            reg(&[
                "add",
                SETTINGS_KEY,
                "/v",
                name,
                "/t",
                kind,
                "/d",
                data,
                "/f"
            ]),
            "reg add {name}"
        );
        Self { name, existed }
    }
}

impl Drop for RegValue {
    fn drop(&mut self) {
        if !self.existed {
            let _ = reg(&["delete", SETTINGS_KEY, "/v", self.name, "/f"]);
        }
    }
}

fn powershell(script: &str) -> String {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "powershell failed: {}\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A root in `CurrentUser\Root`, written as `CryptoAPI`'s registry store holds it (property 32 =
/// the DER; no trust dialog, which would wait for a click on a runner). Removed by thumbprint on
/// drop, pass or fail.
struct UserRoot {
    thumbprint: String,
    key: String,
}

impl UserRoot {
    fn add(der: &[u8], dir: &std::path::Path) -> Self {
        let file = dir.join("interceptor-root.cer");
        std::fs::write(&file, der).unwrap();
        let thumbprint = powershell(&format!(
            "(New-Object System.Security.Cryptography.X509Certificates.X509Certificate2('{}')).Thumbprint",
            file.display()
        ));
        let key =
            format!(r"HKCU:\Software\Microsoft\SystemCertificates\Root\Certificates\{thumbprint}");
        powershell(&format!(
            "$raw = [IO.File]::ReadAllBytes('{file}'); \
             $blob = [byte[]](0x20,0,0,0,1,0,0,0) + [BitConverter]::GetBytes([int]$raw.Length) + $raw; \
             New-Item -Path '{key}' -Force | Out-Null; \
             New-ItemProperty -Path '{key}' -Name Blob -PropertyType Binary -Value $blob -Force | Out-Null",
            file = file.display()
        ));
        let root = Self { thumbprint, key };
        assert!(root.present(), "the root is in the store after the add");
        root
    }

    fn present(&self) -> bool {
        powershell(&format!("Test-Path -LiteralPath '{}'", self.key)) == "True"
    }

    fn remove(&self) {
        powershell(&format!(
            "Remove-Item -LiteralPath '{}' -Recurse -ErrorAction SilentlyContinue",
            self.key
        ));
    }
}

impl Drop for UserRoot {
    fn drop(&mut self) {
        self.remove();
    }
}

fn sha256_hex(der: &[u8]) -> String {
    use std::fmt::Write as _;
    Sha256::digest(der)
        .iter()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// What one run of the rig saw.
struct Seen {
    /// What the guest's `curl` printed: `code=<status>` then the body, or `curl-failed`.
    guest: String,
    /// `GET /api/network-health` after the guest's request.
    health: Value,
    /// The interceptor's NTLM legs, in order.
    legs: Vec<NtlmLeg>,
    interceptor_log: String,
    port: u16,
    root_sha256: String,
}

/// `curl` to the site, no `-k`: the status and the body, or `curl-failed`.
// flake-ok: the name is answered by the interceptor's own table, the machine's resolver is never asked.
const GUEST_CURL: &str = "curl -sS --max-time 60 -o /tmp/chain.body -w 'code=%{http_code}\\n' https://chain.corp.invalid/chain-probe 2>&1 || echo curl-failed; cat /tmp/chain.body 2>/dev/null; true";

/// Creates a workspace that may reach [`SITE`], and waits for its create to end.
async fn create_workspace(vm: &VmHost, api: &Api, tag: &str) -> WorkspaceId {
    let name = format!("{}-{tag}", vm.settings.prefix.as_str());
    let workspace = WorkspaceId::new(&name).unwrap();
    puddle_store::testing::own(vm.host.store(), &workspace);
    vm.host
        .store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(workspace.clone()),
            pattern: Pattern::parse(SITE).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 512}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let name_static: &'static str = Box::leak(name.into_boxed_str());
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    workspace
}

/// Runs the rig; `None` is a skip. `sync_root` is whether the interceptor's root goes into the
/// user's Root store before the host starts (the root sync's input).
async fn run(tag: &str, sync_root: bool) -> Option<Seen> {
    let _registry = registry_test()?;
    let web = WebHost::start(
        &[SITE],
        WebOptions {
            responder: Some(Arc::new(|seen| {
                (seen.target == PROBE).then(|| WebAnswer {
                    status: 200,
                    content_type: "text/plain",
                    body: ANSWER.as_bytes().to_vec(),
                })
            })),
            ..WebOptions::default()
        },
    )
    .await;
    let interceptor =
        Interceptor::start_signing_in_on_ports(&[(SITE.to_owned(), Some(web.tls_addr().port()))])?;
    let port = interceptor.port();
    let root_der = interceptor.root().der().to_vec();

    let scratch = tempfile::tempdir().unwrap();
    let user_root = sync_root.then(|| UserRoot::add(&root_der, scratch.path()));
    let snapshot = read_host_stores().unwrap();
    let platform = HarnessPlatform {
        roots: CorporateRoots::select(&snapshot, SystemTime::now()),
    };
    let vm = start_vm_host_platform(tag, named_id_options(), &platform, |config| {
        config.upstream.discovery.mode = Mode::System;
    })
    .await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace = create_workspace(&vm, &api, tag).await;

    // The interceptor becomes the proxy of the user's Internet Settings, which WinHTTP reports:
    // after the create, so that the image pull is not what meets the interceptor; the host follows
    // the change and the first request below is the first to take the new route.
    let _server = RegValue::set("ProxyServer", "REG_SZ", &format!("127.0.0.1:{port}"));
    let _on = RegValue::set("ProxyEnable", "REG_DWORD", "1");
    let mut followed = false;
    for _ in 0..60 {
        let report = api.get("/api/network-health").await;
        let report: Value = serde_json::from_str(&report.body).unwrap();
        if report["proxy"]["https_proxy"]
            .as_str()
            .is_some_and(|p| p.ends_with(&format!(":{port}")))
        {
            followed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        followed,
        "the host never reported the user's proxy 127.0.0.1:{port}"
    );

    let guest = guest_within(
        &vm.runtime,
        &workspace,
        GUEST_CURL,
        Duration::from_secs(120),
    )
    .await;
    let health = api.get("/api/network-health").await;
    assert_eq!(health.status, 200, "{}", health.body);
    let health: Value = serde_json::from_str(&health.body).unwrap();

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
    drop(vm);

    if let Some(root) = user_root {
        // Removed by thumbprint, and neither the key nor the host's own read finds it any more.
        root.remove();
        assert!(!root.present(), "{} is still in the store", root.thumbprint);
        let after = CorporateRoots::select(&read_host_stores().unwrap(), SystemTime::now());
        assert!(
            after
                .certificates()
                .iter()
                .all(|c| c.der() != root_der.as_slice()),
            "the host still reads the removed root"
        );
    }
    Some(Seen {
        guest,
        health,
        legs: interceptor.legs(),
        interceptor_log: interceptor.log(),
        port,
        root_sha256: sha256_hex(&root_der),
    })
}

fn listed_roots(health: &Value) -> Vec<String> {
    health["roots"]["certificates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["fingerprint"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_company_network_windows_chain_trusts_the_synced_root_and_signs_in_through_sspi() {
    // skip-ok: `run` skips only off a CI runner or without mitmproxy, and fails under PUDDLE_REQUIRE_REGISTRY_TESTS and PUDDLE_REQUIRE_MITMPROXY (the workflow sets both).
    let Some(seen) = run("wchain", true).await else {
        return;
    };
    let log = &seen.interceptor_log;

    // The root sync: the report lists the root the test put in the user's store ...
    assert!(
        listed_roots(&seen.health).contains(&seen.root_sha256),
        "the interceptor's root is not in the report's roots: {}",
        seen.health["roots"]
    );
    // ... the proxy is the one the user's settings name ...
    assert_eq!(
        seen.health["proxy"]["mode"], "system",
        "{}",
        seen.health["proxy"]
    );
    let https_proxy = seen.health["proxy"]["https_proxy"]
        .as_str()
        .unwrap_or_default();
    assert!(
        https_proxy.ends_with(&format!(":{}", seen.port)),
        "the report's proxy is {https_proxy:?}, the interceptor listens on {}: {}",
        seen.port,
        seen.health["proxy"]
    );
    // ... and the guest's curl, without -k, gets the site's own answer through it.
    assert!(
        seen.guest.contains("code=200") && seen.guest.contains(ANSWER),
        "the guest curl: {}\n{log}",
        seen.guest
    );

    // The sign-in: three legs on one connection, accepted as DOMAIN\user with an NTLMv2 answer ...
    let Some(NtlmLeg::Accepted { conn, user, ntlmv2 }) = seen
        .legs
        .iter()
        .rfind(|l| matches!(l, NtlmLeg::Accepted { .. }))
        .cloned()
    else {
        panic!("no accepted Type 3: {:?}\n{log}", seen.legs);
    };
    assert!(ntlmv2 && user.contains('\\'), "{:?}", seen.legs);
    assert!(
        seen.legs
            .iter()
            .any(|l| matches!(l, NtlmLeg::Challenge { conn: c } if *c == conn)),
        "the challenge and the accept are on one connection: {:?}",
        seen.legs
    );
    // ... and the health report says so.
    let attempts = seen.health["sign_in"]["attempts"].as_array().unwrap();
    let ours = attempts
        .iter()
        .find(|a| {
            a["proxy"]
                .as_str()
                .is_some_and(|p| p.ends_with(&format!(":{}", seen.port)))
        })
        .unwrap_or_else(|| panic!("no sign-in attempt to the interceptor: {attempts:?}"));
    assert!(
        matches!(ours["scheme"].as_str(), Some("NTLM" | "Negotiate")),
        "the attempt's scheme: {ours}"
    );
    assert_eq!(ours["result"], "signed_in", "{ours}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_radar_company_network_windows_without_the_root_the_guest_curl_fails() {
    // skip-ok: `run` skips only off a CI runner or without mitmproxy, and fails under PUDDLE_REQUIRE_REGISTRY_TESTS and PUDDLE_REQUIRE_MITMPROXY (the workflow sets both).
    let Some(seen) = run("wnoroot", false).await else {
        return;
    };
    assert!(
        !listed_roots(&seen.health).contains(&seen.root_sha256),
        "the root was never added, yet the report lists it: {}",
        seen.health["roots"]
    );
    assert!(
        !seen.guest.contains("code=200") && !seen.guest.contains(ANSWER),
        "the guest curl reached the site although the interceptor's root was not synced: {}\n{}",
        seen.guest,
        seen.interceptor_log
    );
}
