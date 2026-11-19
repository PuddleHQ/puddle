// SPDX-License-Identifier: AGPL-3.0-or-later
//! The development certificate as a person meets it from a workspace (Windows, a real microVM and
//! the real browsers): an HTTPS server in the guest serves the certificate Puddle placed at
//! `/etc/puddle/dev-cert`, the port is forwarded to this computer, and Edge and Firefox open
//! `https://localhost:<port>/`. Before the person trusts the certificate (the Windows confirmation
//! answered No) both show their certificate warning; after Yes both load the page.
//!
//! This is the step of the first-day playbook that used to be done by hand with
//! `work/scripts/hands-on/devcert-browsers.sh`. `ci/trust-dialog.ps1` presses the button of the
//! Windows confirmation (a throwaway runner has nobody to click) and `ci/browser-cert.mjs` drives the
//! browsers, with the page served by the guest instead of by itself.
//!
//! Named `vm_nightly_*`: the browsers and the confirmation need the runner's desktop session, so
//! `ci/vm-filter.sh` keeps it out of the partitions and the nightly desktop job runs it.
#![cfg(windows)]
#![expect(
    clippy::panic,
    clippy::print_stderr,
    reason = "a VM test: a failed step fails the run; the browsers' output goes to stderr for the log"
)]

mod support;
mod vm_support;

// The dialog answerer and the store helpers of the host-side test of the same certificate.
#[expect(dead_code, reason = "this binary uses part of the helpers")]
#[path = "../../puddle-certs/tests/trust_dialog/mod.rs"]
mod trust_dialog;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use puddle_certs::devcert::GuestDevCert;
use puddle_certs::devcert::{DevCertManager, DevCertStatus, WindowsTrust};
use puddle_host::GuestCertSource;
use puddle_types::WorkspaceId;
use serde_json::json;
use support::{Api, ended};
use trust_dialog::Answer;
use vm_support::{VmHost, guest, start_vm_host_with};

/// The certificate every boot of the guest gets: the one the test's trust manager made, so the one
/// Windows is asked about is the one the guest serves.
struct FixedCert(GuestDevCert);

impl GuestCertSource for FixedCert {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        Some(self.0.clone())
    }
}

impl puddle_api::DevCertService for FixedCert {
    fn status(&self) -> puddle_api::wire::DevCertificate {
        puddle_api::wire::DevCertificate::Unsupported
    }

    fn trust(&self) -> puddle_api::wire::DevCertTrustResult {
        puddle_api::wire::DevCertTrustResult {
            dev_certificate: puddle_api::wire::DevCertificate::Unsupported,
            detail: None,
            waiting: false,
        }
    }
}

/// Edge and Firefox against `url`; `Err` carries the script's output when a browser did not answer
/// `expect`.
fn browsers(expect: &str, url: &str, profiles: &Path) -> Result<String, String> {
    let out = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/browser-cert.mjs"))
        .args([expect, "-", "-"])
        .arg(profiles)
        .env("PUDDLE_BROWSER_URL", url)
        .env("PUDDLE_BROWSERS", "edge,firefox")
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("starting node: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("--- browsers on {url}, expecting {expect} ---\n{text}");
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_nightly_devcert_behind_a_forward_warns_before_trust_and_loads_after_yes_in_edge_and_firefox()
 {
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let manager = Arc::new(DevCertManager::new(data.path(), WindowsTrust));

    // No: the leaf exists and Windows does not trust it.
    let declined = tokio::task::spawn_blocking({
        let manager = manager.clone();
        move || trust_dialog::ensure_answered(&manager, Answer::No)
    })
    .await
    .unwrap()
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(declined.report.status, DevCertStatus::TrustDeclined);
    let der = declined.report.material.cert_der().to_vec();
    let thumbprint = trust_dialog::thumbprint(&der);
    let _remove = trust_dialog::RootGuard::new(&thumbprint);
    assert!(!trust_dialog::current_user_root().contains(&der));

    // The guest boots with that leaf, as it does with the host's own.
    let mut options = support::named_id_options();
    options.dev_cert = Some(Arc::new(FixedCert(GuestDevCert::new(
        &declined.report.material,
    ))));
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_with("devcb", options).await;
    let name = format!("{}-devcb", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
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

    // A server in the guest with the certificate and key Puddle placed there.
    let started = guest(
        &runtime,
        &sandbox,
        "command -v openssl >/dev/null || { echo NO-OPENSSL; exit 0; }
test -s /etc/puddle/dev-cert/localhost.crt || { echo NO-CERTIFICATE; exit 0; }
setsid openssl s_server -accept 8443 -cert /etc/puddle/dev-cert/localhost.crt -key /etc/puddle/dev-cert/localhost.key -www >/tmp/s_server.log 2>&1 </dev/null &
echo $! > /tmp/s_server.pid
sleep 1; echo server-up",
    )
    .await;
    assert!(started.contains("server-up"), "{started}");

    // The forward: this computer's loopback port reaches the guest's 8443.
    let host_port = puddle_test_kit::quiet_port();
    let put = api
        .put(
            &format!("/api/workspaces/{name}/ports/8443"),
            &json!({"host_port": host_port}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    let url = format!("https://localhost:{host_port}/");

    // The control: both browsers warn about the leaf Windows does not trust yet.
    let before = tokio::task::spawn_blocking({
        let (url, profiles) = (url.clone(), work.path().join("before"));
        move || browsers("warning", &url, &profiles)
    })
    .await
    .unwrap()
    .unwrap_or_else(|out| panic!("a browser did not warn about the untrusted leaf: {out}"));
    assert_eq!(before.matches("RESULT ").count(), 2, "{before}");

    // Yes: the same leaf (the manager's folder keeps it), now trusted; both browsers load the page.
    let trusted = tokio::task::spawn_blocking({
        let manager = manager.clone();
        move || trust_dialog::ensure_answered(&manager, Answer::Yes)
    })
    .await
    .unwrap()
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(trusted.report.status, DevCertStatus::Trusted);
    assert_eq!(
        trusted.report.material.cert_der(),
        der,
        "the declined leaf is the one trusted"
    );
    let after = tokio::task::spawn_blocking({
        let (url, profiles) = (url.clone(), work.path().join("after"));
        move || browsers("ok", &url, &profiles)
    })
    .await
    .unwrap()
    .unwrap_or_else(|out| panic!("a browser still warns after Yes: {out}"));
    assert_eq!(after.matches("RESULT ").count(), 2, "{after}");

    guest(
        &runtime,
        &sandbox,
        "kill $(cat /tmp/s_server.pid); echo stopped",
    )
    .await;
    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
