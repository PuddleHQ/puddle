// SPDX-License-Identifier: AGPL-3.0-or-later
//! Do Edge, Chrome and Firefox accept the development certificate once Windows trusts it? On the
//! hosted runner the dialog is answered No first (the control: every browser shows its
//! certificate warning for the same leaf), then Yes, and the same three browsers load the page
//! with no warning. `ci/browser-cert.mjs` serves the leaf and drives the browsers; this test
//! adds it to `CurrentUser\Root` through the product's own trust code and removes it again.
//!
//! The second test is what a person who already ran ASP.NET has: a certificate made and trusted by
//! the real `dotnet dev-certs https`. Its key is exported only encrypted (CNG policy
//! `AllowExport`, no plain export), which the reuse rules refuse, so Puddle passes it over,
//! makes its own leaf, asks its own confirmation, and the same three browsers load a page with
//! that leaf.
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

use puddle_certs::devcert::{
    DevCertManager, DevCertStatus, DevCertTrust, KeyAccess, Origin, SkipReason, WindowsTrust,
};

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

/// Writes `der` and the PKCS#8 `key` as PEM files in `dir` for the browser script.
fn write_pem(dir: &Path, der: &[u8], key: &[u8]) -> (std::path::PathBuf, std::path::PathBuf) {
    let cert_file = dir.join("localhost.crt");
    let key_file = dir.join("localhost.key");
    std::fs::write(
        &cert_file,
        pem::encode(&pem::Pem::new("CERTIFICATE", der.to_vec())),
    )
    .unwrap();
    std::fs::write(
        &key_file,
        pem::encode(&pem::Pem::new("PRIVATE KEY", key.to_vec())),
    )
    .unwrap();
    (cert_file, key_file)
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
    let (cert, key) = write_pem(
        work.path(),
        &der,
        declined.report.material.key().pkcs8_der(),
    );
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

/// Runs `dotnet dev-certs https` with `args` and returns its output. `answer`, when set, presses
/// that button in the Windows trust dialog the command opens (`--trust`). The `dotnet` muxer we
/// start runs the tool in a child process, and that child owns the dialog (the hosted runner
/// showed the dialog under another pid than the one spawned), so the answerer looks at the whole
/// tree.
fn dotnet_dev_certs(args: &[&str], answer: Option<Answer>) -> String {
    let mut child = Command::new("dotnet")
        .args(["dev-certs", "https"])
        .args(args)
        .env("DOTNET_NOLOGO", "1")
        .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("starting dotnet (the Windows runner image has it): {e}"));
    let dialog = answer.map(|a| {
        trust_dialog::run_answerer_for(
            a.word(),
            child.id(),
            true,
            trust_dialog::DIALOG_WAIT.as_secs(),
            None,
        )
        .unwrap_or_else(|e| {
            let _ = child.kill();
            panic!("the dotnet trust dialog was not answered {}: {e}", a.word())
        })
    });
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}{}",
        dialog.unwrap_or_default(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("--- dotnet dev-certs https {args:?} ---\n{text}");
    assert!(
        out.status.success(),
        "dotnet dev-certs https {args:?}: {text}"
    );
    text
}

/// Removes the certificate `dotnet dev-certs https` put in `CurrentUser\My` (and its key) when it
/// goes out of scope, pass or fail; the root copy goes through the [`trust_dialog::RootGuard`].
struct MyGuard(String);

impl Drop for MyGuard {
    fn drop(&mut self) {
        let _ = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(format!(
                "Get-ChildItem Cert:\\CurrentUser\\My | Where-Object Thumbprint -eq '{}' | Remove-Item -DeleteKey",
                self.0
            ))
            .stdin(Stdio::null())
            .output();
    }
}

#[test]
fn a_certificate_from_dotnet_dev_certs_is_passed_over_for_its_key_and_the_own_leaf_loads_in_the_browsers()
 {
    if !enabled() {
        return;
    }
    // The test makes and removes the machine's ASP.NET certificate, so it refuses to run where a
    // person already has one (the hosted runner has none; `PUDDLE_STORE_TESTS` is for a
    // workstation without one).
    let before = WindowsTrust.existing().unwrap();
    assert!(
        before.is_empty(),
        "this machine already has {} ASP.NET development certificate(s); the test makes and removes its own, so it will not run beside yours",
        before.len()
    );

    // What a person who already ran ASP.NET has: the certificate in CurrentUser\My, then trusted
    // (the Windows dialog, answered Yes here).
    dotnet_dev_certs(&[], None);
    let made = WindowsTrust.existing().unwrap();
    assert_eq!(made.len(), 1, "dotnet made one certificate");
    let dotnet_der = made[0].der.clone();
    let dotnet_thumbprint = trust_dialog::thumbprint(&dotnet_der);
    let _my = MyGuard(dotnet_thumbprint.clone());
    let _dotnet_root = trust_dialog::RootGuard::new(&dotnet_thumbprint);
    assert!(
        !WindowsTrust.is_trusted(&dotnet_der).unwrap(),
        "not trusted before --trust"
    );
    let dialog = dotnet_dev_certs(&["--trust"], Some(Answer::Yes));
    trust_dialog::assert_dialog_names(&dialog, &dotnet_thumbprint);
    assert!(WindowsTrust.is_trusted(&dotnet_der).unwrap());

    // dotnet's key is exportable only encrypted, which is why reuse refuses it. The hosted runner's
    // dotnet 10 made it this way; if a dotnet ever makes it plain-exportable, this fails and the
    // reuse path can be tested here with no dialog of Puddle's own.
    let existing = WindowsTrust.existing().unwrap();
    assert_eq!(existing.len(), 1, "{existing:?}");
    assert!(existing[0].trusted);
    let KeyAccess::NotExportable(why) = &existing[0].key else {
        panic!(
            "dotnet's key is now plain-exportable ({:?}): restore the reuse assertions",
            existing[0].key
        );
    };
    assert!(why.contains("plain form"), "{why}");

    // So Puddle passes it over and makes its own leaf, which Windows asks about once.
    let data = tempfile::tempdir().unwrap();
    let manager = Arc::new(DevCertManager::new(data.path(), WindowsTrust));
    let answered =
        trust_dialog::ensure_answered(&manager, Answer::Yes).unwrap_or_else(|e| panic!("{e}"));
    let report = answered.report;
    let der = report.material.cert_der().to_vec();
    let thumbprint = trust_dialog::thumbprint(&der);
    let _leaf_root = trust_dialog::RootGuard::new(&thumbprint);
    assert_ne!(thumbprint, dotnet_thumbprint);
    trust_dialog::assert_dialog_names(&answered.dialog, &thumbprint);
    assert_eq!(report.status, DevCertStatus::Trusted);
    assert_eq!(report.material.origin, Origin::Generated);
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(
        report.skipped[0].fingerprint,
        puddle_certs::Fingerprint::of(&dotnet_der)
    );
    assert!(
        matches!(&report.skipped[0].reason, SkipReason::NotExportable(r) if r == why),
        "{:?}",
        report.skipped[0].reason
    );

    // The leaf and its key serve a page the three browsers load without a warning, with dotnet's
    // trusted certificate beside it in the store.
    let work = tempfile::tempdir().unwrap();
    let (cert, key) = write_pem(work.path(), &der, report.material.key().pkcs8_der());
    let loaded = browsers("ok", &cert, &key, &work.path().join("profiles"))
        .unwrap_or_else(|out| panic!("a browser warns about Puddle's leaf: {out}"));
    assert_eq!(loaded.matches("RESULT ").count(), 3, "{loaded}");
}
