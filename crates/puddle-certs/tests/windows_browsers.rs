// SPDX-License-Identifier: AGPL-3.0-or-later
//! Do Edge, Chrome and Firefox accept the development certificate once Windows trusts it? On the
//! hosted runner the dialog is answered No first (the control: every browser shows its
//! certificate warning for the same leaf), then Yes, and the same three browsers load the page
//! with no warning. `ci/browser-cert.mjs` serves the leaf and drives the browsers; this test
//! adds it to `CurrentUser\Root` through the product's own trust code and removes it again.
//!
//! The second test is what a person who already ran ASP.NET has: a certificate made and trusted by
//! the real `dotnet dev-certs https`. Puddle never uses it: it makes its own leaf, asks its own
//! confirmation, and the same three browsers load a page with that leaf.
//!
//! It runs in the Windows UI job (which has Node and the browsers), where
//! `PUDDLE_REQUIRE_BROWSER_TRUST` is set; elsewhere it says so and passes, unless
//! `PUDDLE_STORE_TESTS` asks for it on a workstation that has the three browsers.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
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

/// The two tests share the certificate store, the trust dialog and the browsers (a second Firefox
/// session is refused), so they run one at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

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
    let _serial = one_at_a_time();
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

/// The thumbprints of the ASP.NET development certificates (the marker extension) in
/// `CurrentUser\My`.
fn dotnet_thumbprints() -> Vec<String> {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(
            "Get-ChildItem Cert:\\CurrentUser\\My | Where-Object { $_.Extensions | \
             Where-Object { $_.Oid.Value -eq '1.3.6.1.4.1.311.84.1.1' } } | \
             ForEach-Object { $_.Thumbprint }",
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "listing the personal store failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Whether `CurrentUser\Root` holds a certificate with `thumbprint`.
fn root_has(thumbprint: &str) -> bool {
    let out = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command"])
        .arg(format!("Test-Path Cert:\\CurrentUser\\Root\\{thumbprint}"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "reading the trusted roots failed");
    String::from_utf8_lossy(&out.stdout).trim() == "True"
}

#[test]
fn puddle_makes_and_trusts_its_own_leaf_beside_a_dotnet_dev_certs_certificate_and_the_browsers_load_it()
 {
    if !enabled() {
        return;
    }
    let _serial = one_at_a_time();
    // The test makes and removes the machine's ASP.NET certificate, so it refuses to run where a
    // person already has one (the hosted runner has none; `PUDDLE_STORE_TESTS` is for a
    // workstation without one).
    let before = dotnet_thumbprints();
    assert!(
        before.is_empty(),
        "this machine already has {} ASP.NET development certificate(s); the test makes and removes its own, so it will not run beside yours",
        before.len()
    );

    // What a person who already ran ASP.NET has: the certificate in CurrentUser\My, then trusted
    // (the Windows dialog, answered Yes here).
    dotnet_dev_certs(&[], None);
    let made = dotnet_thumbprints();
    assert_eq!(made.len(), 1, "dotnet made one certificate: {made:?}");
    let dotnet_thumbprint = made[0].clone();
    let _my = MyGuard(dotnet_thumbprint.clone());
    let _dotnet_root = trust_dialog::RootGuard::new(&dotnet_thumbprint);
    assert!(!root_has(&dotnet_thumbprint), "not trusted before --trust");
    let dialog = dotnet_dev_certs(&["--trust"], Some(Answer::Yes));
    trust_dialog::assert_dialog_names(&dialog, &dotnet_thumbprint);
    assert!(root_has(&dotnet_thumbprint));

    // Puddle makes its own leaf, which Windows asks about once: dotnet's certificate is not used.
    let data = tempfile::tempdir().unwrap();
    let manager = Arc::new(DevCertManager::new(data.path(), WindowsTrust));
    let answered =
        trust_dialog::ensure_answered(&manager, Answer::Yes).unwrap_or_else(|e| panic!("{e}"));
    let report = answered.report;
    let der = report.material.cert_der().to_vec();
    let thumbprint = trust_dialog::thumbprint(&der);
    let _leaf_root = trust_dialog::RootGuard::new(&thumbprint);
    assert!(
        !thumbprint.eq_ignore_ascii_case(&dotnet_thumbprint),
        "Puddle used dotnet's certificate"
    );
    trust_dialog::assert_dialog_names(&answered.dialog, &thumbprint);
    assert_eq!(report.status, DevCertStatus::Trusted);

    // The leaf and its key serve a page the three browsers load without a warning, with dotnet's
    // trusted certificate beside it in the store.
    let work = tempfile::tempdir().unwrap();
    let (cert, key) = write_pem(work.path(), &der, report.material.key().pkcs8_der());
    let loaded = browsers("ok", &cert, &key, &work.path().join("profiles"))
        .unwrap_or_else(|out| panic!("a browser warns about Puddle's leaf: {out}"));
    assert_eq!(loaded.matches("RESULT ").count(), 3, "{loaded}");
}
