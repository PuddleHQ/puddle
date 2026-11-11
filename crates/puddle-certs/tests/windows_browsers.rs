// SPDX-License-Identifier: GPL-3.0-or-later
//! Do Edge, Chrome and Firefox accept the development certificate once Windows trusts it? On the
//! hosted runner the dialog is answered No first (the control: every browser shows its
//! certificate warning for the same leaf), then Yes, and the same three browsers load the page
//! with no warning. `ci/browser-cert.mjs` serves the leaf and drives the browsers; this test
//! adds it to `CurrentUser\Root` through the product's own trust code and removes it again.
//!
//! It runs in the Windows UI job (which has Node and the browsers), where
//! `PUDDLE_REQUIRE_BROWSER_TRUST` is set; elsewhere it says so and passes, unless
//! `PUDDLE_STORE_TESTS` asks for it on a workstation that has the three browsers.
#![cfg(windows)]
#![expect(
    clippy::panic,
    clippy::print_stderr,
    reason = "a failed setup fails the test"
)]

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

use puddle_certs::devcert::{DevCertManager, DevCertStatus, WindowsTrust};

mod trust_dialog;
use trust_dialog::Answer;

fn enabled() -> bool {
    if std::env::var_os("PUDDLE_REQUIRE_BROWSER_TRUST").is_some()
        || std::env::var_os("PUDDLE_STORE_TESTS").is_some()
    {
        return true;
    }
    eprintln!(
        "skipped: needs Edge, Chrome, Firefox and Node, and changes the certificate store; the Windows UI job sets PUDDLE_REQUIRE_BROWSER_TRUST"
    );
    false
}

/// Runs the browsers against `cert` and `key` (PEM) and returns the script's output; `Err` carries
/// it when a browser did not answer `expect`.
fn browsers(expect: &str, cert: &Path, key: &Path, profiles: &Path) -> Result<String, String> {
    let out = Command::new("node")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/browser-cert.mjs"))
        .args([expect])
        .args([cert, key, profiles])
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("starting node: {e}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("--- browsers, expecting {expect} ---\n{text}");
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

#[test]
fn the_browsers_warn_before_the_leaf_is_trusted_and_accept_it_after_yes() {
    if !enabled() {
        return;
    }
    let data = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let manager = Arc::new(DevCertManager::new(data.path(), WindowsTrust));

    // No: the leaf exists, Windows does not trust it, and every browser says so.
    let declined =
        trust_dialog::ensure_answered(&manager, Answer::No).unwrap_or_else(|e| panic!("{e}"));
    let der = declined.report.material.cert_der().to_vec();
    let thumbprint = trust_dialog::thumbprint(&der);
    let _remove = trust_dialog::RootGuard::new(&thumbprint);
    trust_dialog::assert_dialog_names(&declined.dialog, &thumbprint);
    assert!(!trust_dialog::current_user_root().contains(&der));
    assert_eq!(declined.report.status, DevCertStatus::TrustDeclined);
    let cert = work.path().join("localhost.crt");
    let key = work.path().join("localhost.key");
    std::fs::write(
        &cert,
        pem::encode(&pem::Pem::new("CERTIFICATE", der.clone())),
    )
    .unwrap();
    std::fs::write(
        &key,
        pem::encode(&pem::Pem::new(
            "PRIVATE KEY",
            declined.report.material.key().pkcs8_der().to_vec(),
        )),
    )
    .unwrap();
    let control = browsers("warning", &cert, &key, &work.path().join("before"))
        .unwrap_or_else(|out| panic!("a browser did not warn about the untrusted leaf: {out}"));
    assert_eq!(control.matches("RESULT ").count(), 3, "{control}");

    // Yes: the same leaf (the folder keeps it), now trusted, and no browser warns.
    let trusted =
        trust_dialog::ensure_answered(&manager, Answer::Yes).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(trusted.report.status, DevCertStatus::Trusted);
    trust_dialog::assert_dialog_names(&trusted.dialog, &thumbprint);
    assert!(trust_dialog::current_user_root().contains(&der));
    assert_eq!(
        trusted.report.material.cert_der(),
        der,
        "the declined leaf is the one trusted"
    );
    let after = browsers("ok", &cert, &key, &work.path().join("after"))
        .unwrap_or_else(|out| panic!("a browser still warns after Yes: {out}"));
    assert_eq!(after.matches("RESULT ").count(), 3, "{after}");
}
