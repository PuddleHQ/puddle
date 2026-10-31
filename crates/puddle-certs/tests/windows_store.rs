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

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
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

/// The longest one PowerShell call may take. A call is usually 1 to 3 s (a workstation under heavy
/// load only reaches 2 to 3 s), but on the hosted runner the first calls of a job sometimes stall,
/// cause not shown. Of the last 27 runs that printed durations, 4 had a store test over 26 s
/// (slowest 51 s, all passed), and one had two unrelated first calls (an `X509Certificate2` import
/// and a `Get-CimInstance` query) run over 20 s. 60 s is above the slowest whole test seen, so a
/// slow call finishes and a hung one (adding to `CurrentUser\Root` also waits for a confirmation
/// dialog on an interactive desktop) is still killed and named; [`TEST_BUDGET`] keeps a hang
/// inside nextest's 120 s.
const CALL_LIMIT: Duration = Duration::from_secs(60);

/// What one test may spend on waiting for its turn at the stores and on its PowerShell calls
/// together, so that a test with several calls still fails with a named cause before nextest
/// kills the process at 120 s. A call never gets less than [`MIN_CALL`], so a cleanup after a spent
/// budget can still try to remove its certificate.
const TEST_BUDGET: Duration = Duration::from_secs(100);

/// The least a call is given when the test's budget is spent. A cleanup stops at its first hung
/// call, so a spent budget costs at most one more call.
const MIN_CALL: Duration = Duration::from_secs(5);

const _: () = assert!(
    TEST_BUDGET.as_secs() + MIN_CALL.as_secs() < 120,
    "a spent budget plus one more call must end before nextest's 120 s per-test kill"
);

thread_local! {
    /// When this test's budget ends; set when it asks for its turn at the stores. A test that
    /// never takes the lock is bound by [`CALL_LIMIT`] per call only.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// What the next call may take, and whether the test's budget (not [`CALL_LIMIT`]) set it.
fn call_limit() -> (Duration, bool) {
    match DEADLINE.get() {
        Some(deadline) => {
            let left = deadline.saturating_duration_since(Instant::now());
            if left < CALL_LIMIT {
                (left.max(MIN_CALL), true)
            } else {
                (CALL_LIMIT, false)
            }
        }
        None => (CALL_LIMIT, false),
    }
}

/// Held for a test's whole run: a test that reads the stores must not see another test's
/// certificate come and go between two reads. A file lock, so it holds across nextest's test
/// processes and `cargo test`'s threads alike; the OS releases it when a test process dies.
/// The holder's name goes in a second file: Windows refuses reads of a locked file.
struct StoreLock {
    _file: File,
    holder: PathBuf,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.holder);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the lock is shared by every test process, so it must sit at one fixed path"
)]
fn lock_paths() -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir();
    (
        dir.join("puddle-store-tests.lock"),
        dir.join("puddle-store-tests.holder"),
    )
}

/// Who this process is, as the next waiter should see it: nextest runs one test per process and
/// passes its name as an argument, `cargo test` names the thread.
fn who_am_i() -> String {
    format!(
        "pid {}, thread {:?}, arguments {:?}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed"),
        std::env::args().skip(1).collect::<Vec<_>>()
    )
}

/// Takes the lock at `lock`, polling for `wait`; `Err` names the holder written to `holder`.
fn acquire(lock: &Path, holder: &Path, wait: Duration) -> Result<StoreLock, String> {
    let file = File::create(lock).unwrap();
    let start = Instant::now();
    // flake-ok: a bounded retry of taking the lock whose timeout is an `Err` naming the holder (a test checks it), not a panic
    loop {
        match file.try_lock() {
            Ok(()) => {
                std::fs::write(holder, who_am_i()).unwrap();
                return Ok(StoreLock {
                    _file: file,
                    holder: holder.to_owned(),
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => panic!("locking {}: {e}", lock.display()),
        }
        if start.elapsed() >= wait {
            let who = std::fs::read_to_string(holder)
                .unwrap_or_else(|_| "unknown (no holder file)".to_owned());
            return Err(format!(
                "no turn at the certificate stores after {wait:?}; last recorded holder {who} (a process that was killed leaves its record, so it may be gone; otherwise the stores are wedged or the holder is hung in a PowerShell call: see its output)"
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn lock_stores() -> StoreLock {
    DEADLINE.set(Some(Instant::now() + TEST_BUDGET));
    let (lock, holder) = lock_paths();
    acquire(&lock, &holder, TEST_BUDGET).unwrap_or_else(|e| panic!("{e}"))
}

/// The lock when the store tests run here, `None` when they are skipped.
fn enabled() -> Option<StoreLock> {
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

/// Why a child process call failed: a hang is told apart from an exit so a cleanup can stop
/// calling a store that no longer answers.
enum Failure {
    TimedOut(String),
    Exited(String),
}

impl Failure {
    fn into_message(self) -> String {
        match self {
            Self::TimedOut(m) | Self::Exited(m) => m,
        }
    }
}

/// `HH:MM:SS` UTC, so the nextest dump of a hung run shows when the last call started.
fn clock() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        % 86_400;
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Kills `child` and everything it started: a PowerShell killed alone leaves its own children
/// holding the stores for the next test.
fn kill_tree(child: &mut std::process::Child) {
    let _ = Command::new("taskkill.exe")
        .args(["/T", "/F", "/PID", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `command` (`call` names it in messages); `Ok(stdout and stderr)` on exit 0. A call over
/// `limit` is killed with its whole process tree and reported as hung.
fn run_bounded(mut command: Command, call: &str, limit: Duration) -> Result<String, Failure> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    // flake-ok: waits for a child with a limit whose outcome is a killed process tree and an `Err` value the caller matches on, not a panic
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > limit {
            kill_tree(&mut child);
            return Err(Failure::TimedOut(format!(
                "timed out after {limit:?}, process tree killed: {call}"
            )));
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
        Err(Failure::Exited(text))
    }
}

/// Runs a PowerShell script, logging when it starts.
fn powershell_call(script: &str) -> Result<String, Failure> {
    powershell_with("powershell.exe", script)
}

/// [`powershell_call`] with `program` standing in for `powershell.exe`.
fn powershell_with(program: impl AsRef<OsStr>, script: &str) -> Result<String, Failure> {
    let mut command = Command::new(program);
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
        ])
        .arg(script);
    eprintln!("{} powershell: {script}", clock());
    let (limit, by_budget) = call_limit();
    run_bounded(command, script, limit).map_err(|failure| match failure {
        Failure::TimedOut(m) if by_budget => Failure::TimedOut(format!(
            "{m} (the test's {TEST_BUDGET:?} budget for waiting and calls was spent)"
        )),
        other => other,
    })
}

/// Runs a PowerShell script; `Ok(stdout)` on exit 0, `Err(output)` otherwise or on timeout.
fn powershell(script: &str) -> Result<String, String> {
    powershell_call(script).map_err(Failure::into_message)
}

struct TestCert {
    der: Vec<u8>,
    file: PathBuf,
    thumbprint: String,
    /// Owns the folder `file` sits in; none for a `TestCert` that names no real file.
    _dir: Option<tempfile::TempDir>,
}

fn test_root(tag: &str) -> TestCert {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    // flake-ok: a certificate subject, not a path; the store lock runs one test at a time and the tag differs per test
    p.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!("Puddle test root {tag} {nanos}"),
    );
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.not_after = rcgen::date_time_ymd(2040, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let der = p.self_signed(&key).unwrap().der().to_vec();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(format!("puddle-root-sync-{tag}.cer"));
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
        _dir: Some(dir),
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

/// Removes `cert` from every store path given, ignoring "not there". A call that hangs ends the
/// clean-up with `Err` naming the store and thumbprint: the stores are not answering, and each
/// further call would only wait out its own limit.
fn remove_with(
    call: &dyn Fn(&str) -> Result<String, Failure>,
    cert: &TestCert,
    stores: &[&str],
    registry_keys: &[&str],
) -> Result<(), String> {
    let targets = stores
        .iter()
        .map(|store| (*store, format!("{store}\\{}", cert.thumbprint), ""))
        .chain(registry_keys.iter().map(|key| {
            (
                *key,
                format!("{key}\\Certificates\\{}", cert.thumbprint),
                " -Recurse",
            )
        }));
    for (store, path, flags) in targets {
        let script =
            format!("Remove-Item -LiteralPath '{path}'{flags} -ErrorAction SilentlyContinue");
        if let Err(Failure::TimedOut(m)) = call(&script) {
            let _ = std::fs::remove_file(&cert.file);
            return Err(format!(
                "cleanup of store {store}, thumbprint {}: {m}",
                cert.thumbprint
            ));
        }
    }
    let _ = std::fs::remove_file(&cert.file);
    Ok(())
}

struct Cleanup<'a>(Vec<(&'a TestCert, Vec<&'static str>, Vec<&'static str>)>);

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        for (cert, stores, keys) in &self.0 {
            if let Err(hung) = remove_with(&powershell_call, cert, stores, keys) {
                // A panic while another panic unwinds aborts the process, hiding both.
                if std::thread::panicking() {
                    eprintln!("{hung}; this process holds the store lock ({})", who_am_i());
                } else {
                    panic!("{hung}; this process holds the store lock ({})", who_am_i());
                }
                return;
            }
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
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(format!("puddle-devcert-{tag}.cer"));
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
        _dir: Some(dir),
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

// ---- a hung call fails fast and names itself ---------------------------------------------------

/// A child that outlives its parent: `cmd.exe` runs a long `ping`. A kill of `cmd.exe` alone leaves
/// the `ping`, which is what a killed `powershell.exe` does to its own children.
fn hung_command(marker: u32) -> Command {
    use std::os::windows::process::CommandExt;
    let mut command = Command::new("cmd.exe");
    command.raw_arg(format!("/c ping -n {marker} 127.0.0.1 >nul"));
    command
}

fn pings_running(marker: u32) -> usize {
    let listing = powershell(&format!(
        "@(Get-CimInstance Win32_Process -Filter \"Name='PING.EXE'\" | \
         Where-Object {{ $_.CommandLine -like '*-n {marker} *' }}).Count"
    ))
    .unwrap();
    listing.trim().parse().unwrap()
}

#[test]
fn a_hung_call_is_killed_with_its_children_and_named() {
    let marker = 3000 + std::process::id() % 5000;
    let Err(Failure::TimedOut(message)) = run_bounded(
        hung_command(marker),
        "Remove-Item the-hung-call",
        Duration::from_secs(2),
    ) else {
        panic!("a call that outlives its limit must be reported as hung");
    };
    // No time bar: the child pings for 3000 s or more, so a kill that waited for it never returns
    // here, and the test fails by nextest's own timeout instead of passing slowly.
    assert!(message.contains("Remove-Item the-hung-call"), "{message}");
    assert_eq!(
        pings_running(marker),
        0,
        "the child of the killed call is still running"
    );
}

#[test]
fn a_cleanup_call_that_hangs_names_its_store_and_thumbprint_and_stops() {
    let cert = TestCert {
        der: Vec::new(),
        file: std::path::absolute("puddle-cleanup-hang-test.cer").unwrap(),
        thumbprint: "ABCDEF0123".to_owned(),
        _dir: None,
    };
    let calls = std::cell::RefCell::new(Vec::new());
    let hang = |script: &str| {
        calls.borrow_mut().push(script.to_owned());
        Err(Failure::TimedOut("timed out".to_owned()))
    };
    let err = remove_with(
        &hang,
        &cert,
        &["Cert:\\CurrentUser\\My", "Cert:\\CurrentUser\\CA"],
        &[],
    )
    .unwrap_err();
    assert!(err.contains("Cert:\\CurrentUser\\My"), "{err}");
    assert!(err.contains("ABCDEF0123"), "{err}");
    assert_eq!(
        calls.borrow().len(),
        1,
        "no further call to a store that hung"
    );
}

#[test]
fn a_cleanup_that_is_refused_goes_on_to_the_next_store() {
    let cert = TestCert {
        der: Vec::new(),
        file: std::path::absolute("puddle-cleanup-refused-test.cer").unwrap(),
        thumbprint: "ABCDEF0123".to_owned(),
        _dir: None,
    };
    let calls = Cell::new(0);
    let refuse = |_: &str| {
        calls.set(calls.get() + 1);
        Err(Failure::Exited("access denied".to_owned()))
    };
    remove_with(
        &refuse,
        &cert,
        &["Cert:\\CurrentUser\\My"],
        &["HKLM:\\SOFTWARE\\x"],
    )
    .unwrap();
    assert_eq!(calls.get(), 2);
}

#[test]
fn waiting_for_the_stores_ends_naming_the_holder() {
    let dir = tempfile::tempdir().unwrap();
    let lock = dir.path().join("l.lock");
    let holder = dir.path().join("l.holder");
    let first = acquire(&lock, &holder, Duration::from_secs(5)).unwrap();
    // No time bar: `first` is held for the whole test, so an `acquire` that ignored its 1 s limit
    // would never return and nextest's timeout would end the test.
    let err = acquire(&lock, &holder, Duration::from_secs(1))
        .err()
        .expect("the second turn must not be granted while the first is held");
    assert!(
        err.contains(&format!("pid {}", std::process::id())),
        "{err}"
    );
    drop(first);
    assert!(!holder.exists(), "a released lock leaves no holder behind");
    acquire(&lock, &holder, Duration::from_secs(1)).expect("free again after the release");
}

// ---- a slow call is not a hung call, and the test's budget bounds the sum ---------------------

/// A stand-in for `powershell.exe` that takes `seconds`, then prints `finished`. `ping -n N` waits
/// N - 1 seconds.
fn slow_shim(dir: &Path, seconds: u32) -> PathBuf {
    let shim = dir.join("slow-powershell.cmd");
    std::fs::write(
        &shim,
        format!(
            "@echo off\r\nping -n {} 127.0.0.1 >nul\r\necho finished\r\n",
            seconds + 1
        ),
    )
    .unwrap();
    shim
}

/// A runner stalled the first calls of a job for over 20 s and then answered them; failing the
/// test on that was the regression a 20 s limit caused.
#[test]
fn a_call_that_is_slow_but_finishes_does_not_fail_the_test() {
    let dir = tempfile::tempdir().unwrap();
    let shim = slow_shim(dir.path(), 25);
    let start = Instant::now();
    let out = powershell_with(&shim, "Import-Certificate slow-but-fine")
        .unwrap_or_else(|f| panic!("a 25 s call must finish: {}", f.into_message()));
    assert!(out.contains("finished"), "{out}");
    assert!(
        start.elapsed() >= Duration::from_secs(24),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_call_over_the_limit_is_killed_and_the_spent_budget_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let shim = slow_shim(dir.path(), 300);
    DEADLINE.set(Some(Instant::now()));
    // No time bar: the shim sleeps 300 s and the budget is spent, so a call that was not killed
    // at the shortest limit returns `Ok` after 300 s (or nextest ends the test first): never a pass.
    let Err(Failure::TimedOut(message)) = powershell_with(&shim, "Remove-Item the-spent-budget")
    else {
        panic!("a call past the test's budget must be reported as hung");
    };
    assert!(
        message.contains("Remove-Item the-spent-budget"),
        "{message}"
    );
    assert!(message.contains("budget"), "{message}");
}

#[test]
fn a_call_draws_on_what_is_left_of_the_test_budget() {
    DEADLINE.set(None);
    assert_eq!(call_limit(), (CALL_LIMIT, false));
    DEADLINE.set(Some(Instant::now() + TEST_BUDGET));
    assert_eq!(call_limit(), (CALL_LIMIT, false));
    DEADLINE.set(Some(Instant::now() + Duration::from_secs(30)));
    let (limit, by_budget) = call_limit();
    // flake-ok: no elapsed time is measured; `limit` is the 30 s deadline less the microseconds
    // since it was set, so it stays above `MIN_CALL` (5 s) unless the thread stalls for 25 s.
    assert!(
        by_budget && limit <= Duration::from_secs(30) && limit > MIN_CALL,
        "{limit:?}"
    );
    DEADLINE.set(Some(Instant::now()));
    assert_eq!(call_limit(), (MIN_CALL, true));
}
