// SPDX-License-Identifier: GPL-3.0-or-later
//! The real Windows certificate stores (Windows CI job): a root in `CurrentUser\Disallowed` is
//! not exported, machine (Intune-style) and Group Policy roots are, and a `CurrentUser\Root`
//! root is wherever the session may add one (not on the hosted runner, see below).
//!
//! These tests change the stores of the user running them, so they only run where `CI` or
//! `PUDDLE_STORE_TESTS` is set; elsewhere they pass without doing anything. Every certificate
//! they add is removed again, pass or fail. The stores are one state for the whole machine, so
//! the tests take turns: each holds [`lock_stores`] from start to end.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::print_stderr,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

use std::fs::File;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use puddle_certs::{
    CorporateRoots, Fingerprint, Location, Physical, SkipReason, StoreName, StoreSource,
    read_host_stores,
};

const CU_ROOT: StoreSource =
    StoreSource::new(Location::CurrentUser, StoreName::Root, Physical::Default);
const LM_ROOT: StoreSource =
    StoreSource::new(Location::LocalMachine, StoreName::Root, Physical::Default);
const LM_ROOT_GP: StoreSource = StoreSource::new(
    Location::LocalMachine,
    StoreName::Root,
    Physical::GroupPolicy,
);

/// Adding to `CurrentUser\Root` asks the user to confirm in a dialog on an interactive desktop;
/// a hung dialog must fail the test, not hang the job.
const POWERSHELL_LIMIT: Duration = Duration::from_secs(90);

/// Held for a test's whole run: a test that reads the stores must not see another test's
/// certificate come and go between two reads. A file lock, so it holds across nextest's test
/// processes and `cargo test`'s threads alike; the OS releases it when a test process dies.
fn lock_stores() -> File {
    let file = File::create(std::env::temp_dir().join("puddle-store-tests.lock")).unwrap();
    file.lock().unwrap();
    file
}

/// The lock when the store tests run here, `None` when they are skipped.
fn enabled() -> Option<File> {
    let on = std::env::var_os("CI").is_some() || std::env::var_os("PUDDLE_STORE_TESTS").is_some();
    if !on {
        // The tests edit the machine's certificate stores, so they run where CI is set or where the
        // person asked with PUDDLE_STORE_TESTS=1; the Windows job sets PUDDLE_REQUIRE_STORE_TESTS to
        // fail if it ever stops qualifying.
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_STORE_TESTS").is_none(),
            "PUDDLE_REQUIRE_STORE_TESTS is set but the store tests are not enabled (set CI or PUDDLE_STORE_TESTS)"
        );
        eprintln!("skipped: changes the certificate stores; set PUDDLE_STORE_TESTS=1 to run");
        return None;
    }
    Some(lock_stores())
}

/// Runs a PowerShell script; `Ok(stdout)` on exit 0, `Err(output)` otherwise or on timeout.
fn powershell(script: &str) -> Result<String, String> {
    let mut child = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > POWERSHELL_LIMIT {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            return Err(format!(
                "timed out after {POWERSHELL_LIMIT:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

struct TestCert {
    der: Vec<u8>,
    file: PathBuf,
    thumbprint: String,
}

fn test_root(tag: &str) -> TestCert {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("Puddle test root {tag} {nanos}"),
    );
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.not_after = rcgen::date_time_ymd(2040, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let der = p.self_signed(&key).unwrap().der().to_vec();
    let file = std::env::temp_dir().join(format!("puddle-root-sync-{tag}-{nanos}.cer"));
    std::fs::write(&file, &der).unwrap();
    let thumbprint = powershell(&format!(
        "(New-Object System.Security.Cryptography.X509Certificates.X509Certificate2('{}')).Thumbprint",
        file.display()
    ))
    .unwrap()
    .trim()
    .to_owned();
    TestCert {
        der,
        file,
        thumbprint,
    }
}

/// Imports `cert` into `store` (`Cert:\\...` path); `Err` with PowerShell's output when refused.
fn try_add(cert: &TestCert, store: &str) -> Result<(), String> {
    powershell(&format!(
        "Import-Certificate -FilePath '{}' -CertStoreLocation '{store}' | Out-Null",
        cert.file.display()
    ))
    .map(drop)
}

fn add(cert: &TestCert, store: &str) {
    try_add(cert, store).unwrap_or_else(|e| panic!("import into {store}: {e}"));
}

/// Writes `cert` as a serialized certificate (property 32 = the DER) under `key`, the way
/// `CryptoAPI`'s registry stores hold it; Group Policy writes these the same way.
fn write_registry_blob(cert: &TestCert, key: &str) {
    let script = format!(
        "$raw = [IO.File]::ReadAllBytes('{file}'); \
         $blob = [byte[]](0x20,0,0,0,1,0,0,0) + [BitConverter]::GetBytes([int]$raw.Length) + $raw; \
         $k = '{key}\\Certificates\\{t}'; \
         New-Item -Path $k -Force | Out-Null; \
         New-ItemProperty -Path $k -Name Blob -PropertyType Binary -Value $blob -Force | Out-Null",
        file = cert.file.display(),
        t = cert.thumbprint,
    );
    powershell(&script).unwrap_or_else(|e| panic!("registry write under {key}: {e}"));
}

/// Removes `cert` from every store path given, ignoring "not there".
fn remove(cert: &TestCert, stores: &[&str], registry_keys: &[&str]) {
    for store in stores {
        let _ = powershell(&format!(
            "Remove-Item -LiteralPath '{store}\\{}' -ErrorAction SilentlyContinue",
            cert.thumbprint
        ));
    }
    for key in registry_keys {
        let _ = powershell(&format!(
            "Remove-Item -LiteralPath '{key}\\Certificates\\{}' -Recurse -ErrorAction SilentlyContinue",
            cert.thumbprint
        ));
    }
    let _ = std::fs::remove_file(&cert.file);
}

struct Cleanup<'a>(Vec<(&'a TestCert, Vec<&'static str>, Vec<&'static str>)>);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for (cert, stores, keys) in &self.0 {
            remove(cert, stores, keys);
        }
    }
}

fn select() -> CorporateRoots {
    let snapshot = read_host_stores().unwrap();
    for u in snapshot.unreadable() {
        eprintln!("unreadable store {}: {}", u.source, u.reason);
    }
    CorporateRoots::select(&snapshot, SystemTime::now())
}

fn synced_sources(roots: &CorporateRoots, der: &[u8]) -> Option<Vec<StoreSource>> {
    let fp = Fingerprint::of(der);
    roots
        .certificates()
        .iter()
        .find(|c| c.fingerprint() == fp)
        .map(|c| c.sources().to_vec())
}

const LM_GP_ROOT_KEY: &str = "HKLM:\\SOFTWARE\\Policies\\Microsoft\\SystemCertificates\\Root";

/// `CurrentUser\\Root` needs the user to confirm each added root in a dialog; a non-interactive
/// session (the hosted runner) gets "UI is not allowed in this operation", and a root written to
/// the store's registry key directly is not enumerated. So this test cannot run on a runner: it is
/// ignored, and a person on a workstation runs it by hand (someone clicks Yes), where a refused
/// import fails it.
#[test]
#[ignore = "adding a root to CurrentUser\\Root asks for confirmation in a dialog, which a hosted runner refuses; run it by hand on a workstation with --run-ignored"]
fn a_current_user_root_is_exported_when_one_can_be_added() {
    let Some(_stores) = enabled() else {
        return;
    };
    let trusted = test_root("user");
    let _cleanup = Cleanup(vec![(&trusted, vec!["Cert:\\CurrentUser\\Root"], vec![])]);
    try_add(&trusted, "Cert:\\CurrentUser\\Root")
        .unwrap_or_else(|e| panic!("CurrentUser\\Root refused the import: {}", e.trim()));
    assert_eq!(
        synced_sources(&select(), &trusted.der),
        Some(vec![CU_ROOT]),
        "the CurrentUser root is not exported"
    );
}

#[test]
fn a_root_in_current_user_disallowed_is_not_exported() {
    let Some(_stores) = enabled() else {
        return;
    };
    let banned = test_root("banned");
    let _cleanup = Cleanup(vec![(
        &banned,
        vec![
            "Cert:\\LocalMachine\\Root",
            "Cert:\\CurrentUser\\Disallowed",
        ],
        vec![],
    )]);
    add(&banned, "Cert:\\LocalMachine\\Root");
    let roots = select();
    assert_eq!(
        synced_sources(&roots, &banned.der),
        Some(vec![LM_ROOT]),
        "control: trusted before it is distrusted"
    );
    add(&banned, "Cert:\\CurrentUser\\Disallowed");
    let roots = select();
    assert_eq!(
        synced_sources(&roots, &banned.der),
        None,
        "a Disallowed root was exported"
    );
    let skipped = roots
        .skipped()
        .iter()
        .find(|s| s.fingerprint == Fingerprint::of(&banned.der))
        .expect("the Disallowed root is listed as skipped");
    assert_eq!(skipped.reason, SkipReason::Disallowed);
    eprintln!(
        "{} certificates synced, {} left out",
        roots.certificates().len(),
        roots.skipped().len()
    );
}

#[test]
fn machine_and_group_policy_roots_are_exported() {
    let Some(_stores) = enabled() else {
        return;
    };
    let intune = test_root("machine");
    let gpo = test_root("gpo");
    let _cleanup = Cleanup(vec![
        (&intune, vec!["Cert:\\LocalMachine\\Root"], vec![]),
        (&gpo, vec![], vec![LM_GP_ROOT_KEY]),
    ]);
    // Needs an elevated runner (GitHub's is); Intune puts its roots in LocalMachine\Root.
    add(&intune, "Cert:\\LocalMachine\\Root");
    // A GPO-pushed root, written where Group Policy writes it.
    write_registry_blob(&gpo, LM_GP_ROOT_KEY);

    let roots = select();
    assert_eq!(synced_sources(&roots, &intune.der), Some(vec![LM_ROOT]));
    assert_eq!(synced_sources(&roots, &gpo.der), Some(vec![LM_ROOT_GP]));
}

#[test]
fn reading_the_stores_twice_gives_the_same_selection() {
    let _stores = lock_stores();
    let now = SystemTime::now();
    let one = CorporateRoots::select(&read_host_stores().unwrap(), now);
    let two = CorporateRoots::select(&read_host_stores().unwrap(), now);
    assert_eq!(one, two);
}

// ---- the development certificate ---------------------------------------------------------------

use puddle_certs::devcert::{
    DevCertManager, DevCertStatus, DevCertTrust, KeyAccess, Origin, WindowsTrust,
};

/// A certificate with the ASP.NET marker and a CNG key in `CurrentUser\My`, made like `dotnet
/// dev-certs` makes its own. `policy` is `New-SelfSignedCertificate`'s `-KeyExportPolicy`:
/// `Exportable` allows plain export, `ExportableEncrypted` and `NonExportable` do not.
fn dev_cert(tag: &str, policy: &str) -> TestCert {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let file = std::env::temp_dir().join(format!("puddle-devcert-{tag}-{nanos}.cer"));
    // `Stop` makes a failed cmdlet fail the script: without it a failed New-SelfSignedCertificate
    // left `$c` empty, Export-Certificate wrote no file and the script still exited 0.
    let thumbprint = powershell(&format!(
        "$ErrorActionPreference = 'Stop'; \
         $c = New-SelfSignedCertificate -Subject 'CN=puddle devcert {tag} {nanos}' -DnsName localhost \
           -KeyAlgorithm ECDSA_nistP256 -KeyExportPolicy {policy} -CertStoreLocation Cert:\\CurrentUser\\My \
           -NotAfter (Get-Date).AddDays(90) -TextExtension @('1.3.6.1.4.1.311.84.1.1={{hex}}06'); \
         Export-Certificate -Cert $c -FilePath '{}' | Out-Null; \
         $c.Thumbprint",
        file.display()
    ))
    .unwrap_or_else(|e| panic!("making a development certificate: {e}"))
    .trim()
    .to_owned();
    assert!(!thumbprint.is_empty(), "no thumbprint for {tag}");
    let der = std::fs::read(&file).unwrap();
    TestCert {
        der,
        file,
        thumbprint,
    }
}

const CU_MY: &str = "Cert:\\CurrentUser\\My";
/// `New-SelfSignedCertificate` also leaves a copy in the user's intermediate store, which the
/// corporate-root selection reads; the cleanup removes it there too.
const CU_CA: &str = "Cert:\\CurrentUser\\CA";

#[test]
fn an_exportable_development_certificate_is_found_and_its_key_exports() {
    let Some(_stores) = enabled() else {
        return;
    };
    let cert = dev_cert("exportable", "Exportable");
    let _cleanup = Cleanup(vec![(&cert, vec![CU_MY, CU_CA], vec![])]);

    let found = WindowsTrust.existing().unwrap();
    let ours = found
        .iter()
        .find(|c| c.der == cert.der)
        .expect("the certificate in CurrentUser\\My is listed");
    assert_eq!(ours.key, KeyAccess::Exportable);
    assert!(!ours.trusted, "it is in My only");

    let key = WindowsTrust.export_key(&cert.der).unwrap();
    // PKCS#8 (RFC 5958): SEQUENCE { INTEGER version, SEQUENCE algorithm, OCTET STRING key,
    // [0] attributes OPTIONAL, [1] public key OPTIONAL }. CNG adds optional trailing fields
    // (the first CI run saw four elements), so the three required ones are checked and the rest
    // may only be those two context tags.
    let (_, parsed) = x509_parser::der_parser::parse_der(key.pkcs8_der()).unwrap();
    let items = parsed.as_sequence().unwrap();
    let tags: Vec<_> = items.iter().map(|i| i.header.tag().0).collect();
    eprintln!("exported PKCS#8 element tags {tags:?}");
    assert!(
        (3..=5).contains(&items.len()),
        "a PrivateKeyInfo: tags {tags:?}"
    );
    assert_eq!(items[0].as_u32().unwrap(), 0, "version");
    assert!(items[1].as_sequence().is_ok(), "algorithm identifier");
    assert!(
        items[2].as_slice().is_ok_and(|k| !k.is_empty()),
        "private key"
    );
    for extra in &items[3..] {
        assert!(
            extra.header.class() == x509_parser::der_parser::ber::Class::ContextSpecific
                && extra.header.tag().0 <= 1,
            "optional trailing field: tags {tags:?}"
        );
    }
    // ring is strict about the key's layout; when it accepts it, it must be the certificate's key.
    if let Ok(pair) = rcgen::KeyPair::try_from(key.pkcs8_der()) {
        let (_, x509) = x509_parser::parse_x509_certificate(&cert.der).unwrap();
        assert_eq!(
            pair.public_key_raw(),
            x509.public_key().subject_public_key.data.as_ref()
        );
    } else {
        eprintln!("note: the exported PKCS#8 is not in ring's layout (valid for OpenSSL and .NET)");
    }
}

#[test]
fn keys_that_cannot_be_exported_in_plain_form_are_reported_so() {
    let Some(_stores) = enabled() else {
        return;
    };
    for policy in ["ExportableEncrypted", "NonExportable"] {
        let cert = dev_cert(policy, policy);
        let _cleanup = Cleanup(vec![(&cert, vec![CU_MY, CU_CA], vec![])]);
        let found = WindowsTrust.existing().unwrap();
        let ours = found.iter().find(|c| c.der == cert.der).unwrap();
        assert!(
            matches!(ours.key, KeyAccess::NotExportable(_)),
            "{policy}: {:?}",
            ours.key
        );
        assert!(WindowsTrust.export_key(&cert.der).is_err(), "{policy}");
    }
}

#[test]
fn a_trusted_exportable_development_certificate_is_reused() {
    let Some(_stores) = enabled() else {
        return;
    };
    let cert = dev_cert("reuse", "Exportable");
    // Machine roots are part of the user's trusted roots; the runner is elevated.
    let _cleanup = Cleanup(vec![(
        &cert,
        vec![CU_MY, CU_CA, "Cert:\\LocalMachine\\Root"],
        vec![],
    )]);
    add(&cert, "Cert:\\LocalMachine\\Root");

    let data = tempfile::tempdir().unwrap();
    let manager = DevCertManager::new(data.path(), WindowsTrust);
    assert_eq!(
        manager.status(SystemTime::now()),
        DevCertStatus::ReusingExisting
    );
    let report = manager.ensure(SystemTime::now()).unwrap();
    assert_eq!(report.status, DevCertStatus::ReusingExisting);
    assert_eq!(report.material.origin, Origin::ReusedAspNet);
    assert_eq!(report.material.cert_der(), cert.der);
    assert_ne!(report.material.key().pkcs8_der().len(), 0);
    assert!(
        !data.path().join(puddle_certs::devcert::FOLDER).exists(),
        "reusing makes no leaf"
    );
}

#[test]
fn the_machine_roots_count_as_trusted_and_an_unknown_certificate_does_not() {
    let Some(_stores) = enabled() else {
        return;
    };
    let snapshot = read_host_stores().unwrap();
    let machine_root = snapshot
        .certs()
        .iter()
        .find(|c| c.source == LM_ROOT)
        .expect("a machine root");
    assert!(WindowsTrust.is_trusted(&machine_root.der).unwrap());
    assert!(!WindowsTrust.is_trusted(&test_root("never").der).unwrap());
}

/// The generated leaf is offered to `CurrentUser\Root`. A person is asked once there; the hosted
/// runner is refused ("UI is not allowed"), so this only checks that every answer is a reported
/// state with the leaf still handed over, and removes the leaf if the runner did add it.
///
/// Ignored on the runner: the add is in-process (`CertAddCertificateContextToStore`), so unlike
/// the PowerShell import it is not refused: it opens the Windows confirmation dialog on the
/// runner's desktop and waits for a click nobody gives (the first CI run hit the 120 s limit).
/// That the dialog is the only thing in the way is a person's check: run it by hand on a
/// workstation and click No or Yes. The states themselves are tested over a fake store.
#[test]
#[ignore = "adding the leaf to CurrentUser\\Root opens a confirmation dialog nobody clicks on a hosted runner; run it by hand on a workstation with --run-ignored"]
fn asking_for_trust_ends_in_a_reported_state_never_a_crash() {
    let Some(_stores) = enabled() else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let manager = DevCertManager::new(data.path(), WindowsTrust);
    let report = manager.ensure(SystemTime::now()).unwrap();
    // Remove the leaf from the trusted roots again, whatever the outcome.
    let thumbprint = powershell(&format!(
        "$f = Join-Path $env:TEMP 'puddle-leaf-{}.cer'; [IO.File]::WriteAllBytes($f, [byte[]]({})); \
         (New-Object Security.Cryptography.X509Certificates.X509Certificate2($f)).Thumbprint; Remove-Item $f",
        std::process::id(),
        report
            .material
            .cert_der()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(",")
    ))
    .unwrap()
    .trim()
    .to_owned();
    let _ = powershell(&format!(
        "Remove-Item -LiteralPath 'Cert:\\CurrentUser\\Root\\{thumbprint}' -ErrorAction SilentlyContinue"
    ));
    eprintln!("status {:?}, detail {:?}", report.status, report.detail);
    assert!(matches!(
        report.status,
        DevCertStatus::Trusted | DevCertStatus::TrustDeclined | DevCertStatus::TrustFailed
    ));
    assert_eq!(report.material.origin, Origin::Generated);
}
