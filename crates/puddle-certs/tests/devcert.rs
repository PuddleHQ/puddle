// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate over a fake host store: which existing certificate is reused and
//! why the others are skipped, the generated leaf's life (kept, renewed, replaced when exposed),
//! and the states trust can end in. The real Windows store is `windows_store.rs`.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns: a failed setup fails the test"
)]

use std::fs;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use puddle_certs::Fingerprint;
use puddle_certs::devcert::leaf::{self, ASPNET_MARKER_OID, DNS_NAMES, IP_ADDRESSES};
use puddle_certs::devcert::{
    DevCertError, DevCertManager, DevCertStatus, DevCertTrust, ExistingCert, KeyAccess, LEAF_FILE,
    Origin, PrivateKey, SkipReason, TrustError, TrustOutcome, Unsupported,
};
use rcgen::{
    BasicConstraints, CertificateParams, CustomExtension, DistinguishedName, DnType, IsCa, KeyPair,
    SanType,
};
use x509_parser::prelude::{FromDer, X509Certificate};

const DAY: Duration = Duration::from_hours(24);

/// A fixed "now", far from the epoch so negative offsets stay positive.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_hours(500_000)
}

/// What an existing certificate looks like in a test.
#[derive(Clone)]
struct Spec {
    marker: Option<u8>,
    ca: bool,
    /// Days before now the validity starts (negative: in the future).
    started: i64,
    /// Days after now it ends.
    ends: i64,
    /// Seconds added to the end (edge tests).
    end_secs: i64,
    key: KeyAccess,
    trusted: bool,
    /// DNS names in the certificate.
    names: Vec<&'static str>,
}

impl Spec {
    fn good() -> Self {
        Self {
            marker: Some(6),
            ca: false,
            started: 10,
            ends: 200,
            end_secs: 0,
            key: KeyAccess::Exportable,
            trusted: true,
            names: vec!["localhost", "*.dev.localhost"],
        }
    }

    fn build(&self) -> ExistingCert {
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "localhost");
        params.subject_alt_names = self
            .names
            .iter()
            .map(|n| SanType::DnsName((*n).try_into().unwrap()))
            .collect();
        params.is_ca = if self.ca {
            IsCa::Ca(BasicConstraints::Unconstrained)
        } else {
            IsCa::ExplicitNoCa
        };
        if let Some(version) = self.marker {
            params.custom_extensions = vec![CustomExtension::from_oid_content(
                &ASPNET_MARKER_OID,
                vec![version],
            )];
        }
        let day = |days: i64| time::Duration::days(days);
        let t0 = time::OffsetDateTime::from(now());
        params.not_before = t0 - day(self.started);
        params.not_after = t0 + day(self.ends) + time::Duration::seconds(self.end_secs);
        let key = KeyPair::generate().unwrap();
        let der = params.self_signed(&key).unwrap().der().to_vec();
        ExistingCert {
            der,
            key: self.key.clone(),
            trusted: self.trusted,
        }
    }
}

/// A certificate and the key `export_key` returns for it (`None`: the export fails).
type Exported = (Vec<u8>, Option<Vec<u8>>);

/// A fake store: the user's certificates with keys, the trusted roots, and what a trust request
/// answers. Counts calls so tests can say what was never asked.
struct FakeTrust {
    existing: Vec<ExistingCert>,
    /// The key bytes `export_key` returns; `None` makes it fail.
    exported: Mutex<Vec<Exported>>,
    trusted: Mutex<Vec<Vec<u8>>>,
    answer: Mutex<Result<TrustOutcome, TrustError>>,
    trust_calls: Mutex<Vec<Vec<u8>>>,
    export_calls: Mutex<u32>,
}

impl FakeTrust {
    fn new(existing: Vec<ExistingCert>) -> Self {
        let exported = existing
            .iter()
            .map(|c| (c.der.clone(), Some(b"fake-existing-key-bytes".to_vec())))
            .collect();
        Self {
            existing,
            exported: Mutex::new(exported),
            trusted: Mutex::new(Vec::new()),
            answer: Mutex::new(Ok(TrustOutcome::Trusted)),
            trust_calls: Mutex::new(Vec::new()),
            export_calls: Mutex::new(0),
        }
    }

    fn answering(self, answer: Result<TrustOutcome, TrustError>) -> Self {
        *self.answer.lock().unwrap() = answer;
        self
    }

    fn trust_calls(&self) -> usize {
        self.trust_calls.lock().unwrap().len()
    }
}

impl DevCertTrust for &FakeTrust {
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
        Ok(self.existing.clone())
    }

    fn export_key(&self, cert_der: &[u8]) -> Result<PrivateKey, TrustError> {
        *self.export_calls.lock().unwrap() += 1;
        let exported = self.exported.lock().unwrap();
        match exported.iter().find(|(der, _)| der == cert_der) {
            Some((_, Some(key))) => Ok(PrivateKey::from_pkcs8_der(key.clone())),
            _ => Err(TrustError::Failed("os error 0x8009000b".into())),
        }
    }

    fn is_trusted(&self, cert_der: &[u8]) -> Result<bool, TrustError> {
        Ok(self.trusted.lock().unwrap().iter().any(|t| t == cert_der))
    }

    fn trust(&self, cert_der: &[u8]) -> Result<TrustOutcome, TrustError> {
        self.trust_calls.lock().unwrap().push(cert_der.to_vec());
        let answer = self.answer.lock().unwrap().clone();
        if answer == Ok(TrustOutcome::Trusted) {
            self.trusted.lock().unwrap().push(cert_der.to_vec());
        }
        answer
    }
}

fn manager<'a>(dir: &tempfile::TempDir, trust: &'a FakeTrust) -> DevCertManager<&'a FakeTrust> {
    DevCertManager::new(dir.path(), trust)
}

fn leaf_path(dir: &tempfile::TempDir) -> std::path::PathBuf {
    dir.path()
        .join(puddle_certs::devcert::FOLDER)
        .join(LEAF_FILE)
}

fn parse(der: &[u8]) -> X509Certificate<'_> {
    X509Certificate::from_der(der).unwrap().1
}

// ---- reuse ------------------------------------------------------------------------------------

#[test]
fn a_trusted_exportable_valid_certificate_is_reused_without_any_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let existing = Spec::good().build();
    let fake = FakeTrust::new(vec![existing.clone()]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();

    assert_eq!(report.status, DevCertStatus::ReusingExisting);
    assert_eq!(report.material.origin, Origin::ReusedAspNet);
    assert_eq!(report.material.cert_der(), existing.der);
    assert_eq!(
        report.material.key().pkcs8_der(),
        b"fake-existing-key-bytes"
    );
    assert_eq!(report.skipped.len(), 0);
    assert_eq!(fake.trust_calls(), 0, "reuse never asks for trust");
    assert!(!leaf_path(&dir).exists(), "nothing generated when reusing");
}

#[test]
fn a_certificate_without_basic_constraints_is_a_leaf_not_a_ca() {
    // Windows treats a certificate that says nothing as an end-entity certificate.
    let dir = tempfile::tempdir().unwrap();
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::DnsName("localhost".try_into().unwrap())];
    params.is_ca = IsCa::NoCa;
    params.custom_extensions = vec![CustomExtension::from_oid_content(
        &ASPNET_MARKER_OID,
        vec![6],
    )];
    params.not_before = time::OffsetDateTime::from(now() - DAY);
    params.not_after = time::OffsetDateTime::from(now() + DAY * 200);
    let der = params
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap()
        .der()
        .to_vec();
    assert_eq!(leaf::inspect(&der).unwrap().basic_constraints, None);
    let fake = FakeTrust::new(vec![ExistingCert {
        der: der.clone(),
        key: KeyAccess::Exportable,
        trusted: true,
    }]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.material.cert_der(), der);
}

#[test]
fn certificates_that_fall_short_are_skipped_with_the_reason() {
    let cases: Vec<(&str, Spec, SkipReason)> = vec![
        (
            "expired",
            Spec {
                ends: -1,
                ..Spec::good()
            },
            SkipReason::Expired,
        ),
        (
            "not yet valid",
            Spec {
                started: -2,
                ..Spec::good()
            },
            SkipReason::NotYetValid,
        ),
        (
            "ends in 29 days 23 h 59 min 59 s",
            Spec {
                ends: 30,
                end_secs: -1,
                ..Spec::good()
            },
            SkipReason::ExpiresSoon,
        ),
        (
            "no key",
            Spec {
                key: KeyAccess::Missing,
                ..Spec::good()
            },
            SkipReason::NoKey,
        ),
        (
            "key not exportable",
            Spec {
                key: KeyAccess::NotExportable("encrypted export only".into()),
                ..Spec::good()
            },
            SkipReason::NotExportable("encrypted export only".into()),
        ),
        (
            "not trusted",
            Spec {
                trusted: false,
                ..Spec::good()
            },
            SkipReason::NotTrusted,
        ),
        (
            "valid for a real domain",
            Spec {
                names: vec!["localhost", "*.example.com"],
                ..Spec::good()
            },
            SkipReason::NamesTooWide("*.example.com".into()),
        ),
        (
            "valid for no name",
            Spec {
                names: vec![],
                ..Spec::good()
            },
            SkipReason::NamesTooWide("(none)".into()),
        ),
        (
            "a CA",
            Spec {
                ca: true,
                ..Spec::good()
            },
            SkipReason::NotALeaf,
        ),
    ];
    for (name, spec, reason) in cases {
        let dir = tempfile::tempdir().unwrap();
        let cert = spec.build();
        let fake = FakeTrust::new(vec![cert.clone()]);
        let report = manager(&dir, &fake).ensure(now()).unwrap();

        assert_eq!(report.material.origin, Origin::Generated, "{name}");
        assert_eq!(report.skipped.len(), 1, "{name}");
        assert_eq!(report.skipped[0].reason, reason, "{name}");
        assert_eq!(
            report.skipped[0].fingerprint,
            Fingerprint::of(&cert.der),
            "{name}"
        );
        assert!(
            !report.skipped[0].reason.to_string().is_empty(),
            "{name}: the reason has words"
        );
    }
}

#[test]
fn exactly_thirty_days_left_is_still_reused() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![
        Spec {
            ends: 30,
            ..Spec::good()
        }
        .build(),
    ]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::ReusingExisting);
}

#[test]
fn a_certificate_without_the_marker_is_not_a_development_certificate_and_is_not_listed() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![
        Spec {
            marker: None,
            ..Spec::good()
        }
        .build(),
    ]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.material.origin, Origin::Generated);
    assert_eq!(report.skipped.len(), 0);
}

#[test]
fn the_highest_marker_version_wins_then_the_latest_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let old_version = Spec {
        marker: Some(4),
        ends: 360,
        ..Spec::good()
    }
    .build();
    let short = Spec {
        ends: 100,
        ..Spec::good()
    }
    .build();
    let long = Spec {
        ends: 300,
        ..Spec::good()
    }
    .build();
    let fake = FakeTrust::new(vec![old_version, short, long.clone()]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.material.cert_der(), long.der);
}

#[test]
fn a_key_that_will_not_export_falls_through_to_the_next_certificate_and_then_to_a_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let better = Spec {
        ends: 300,
        ..Spec::good()
    }
    .build();
    let other = Spec {
        ends: 100,
        ..Spec::good()
    }
    .build();
    let fake = FakeTrust::new(vec![better.clone(), other.clone()]);
    fake.exported
        .lock()
        .unwrap()
        .iter_mut()
        .find(|(der, _)| *der == better.der)
        .unwrap()
        .1 = None;
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(
        report.material.cert_der(),
        other.der,
        "the second one is used"
    );
    assert!(matches!(
        report.skipped.as_slice(),
        [s] if s.fingerprint == Fingerprint::of(&better.der)
            && matches!(&s.reason, SkipReason::ExportFailed(why) if why.contains("0x8009000b"))
    ));

    // Neither exports: Puddle's own leaf takes over.
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![better.clone()]);
    fake.exported.lock().unwrap()[0].1 = None;
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.material.origin, Origin::Generated);
    assert_eq!(report.skipped.len(), 1);
}

#[test]
fn status_reads_only() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);
    assert_eq!(m.status(now()), DevCertStatus::NeedsTrust);
    assert_eq!(fake.trust_calls(), 0);
    assert_eq!(*fake.export_calls.lock().unwrap(), 0);
    assert!(
        !dir.path().join(puddle_certs::devcert::FOLDER).exists(),
        "status makes nothing"
    );

    let fake = FakeTrust::new(vec![Spec::good().build()]);
    let m = manager(&dir, &fake);
    assert_eq!(m.status(now()), DevCertStatus::ReusingExisting);
    assert_eq!(
        *fake.export_calls.lock().unwrap(),
        0,
        "no key is exported to answer"
    );
}

// ---- the generated leaf -----------------------------------------------------------------------

#[test]
fn the_generated_leaf_has_the_documented_shape() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    let der = report.material.cert_der();
    let cert = parse(der);

    // Basic constraints: present, critical, CA false.
    let bc = cert
        .extensions()
        .iter()
        .find(|e| e.oid == x509_parser::oid_registry::OID_X509_EXT_BASIC_CONSTRAINTS)
        .unwrap();
    assert!(bc.critical);
    let (critical, ca) = leaf::inspect(der).unwrap().basic_constraints.unwrap();
    assert!(critical && !ca);

    // Key usage: no certificate signing; server authentication only.
    let ku = cert.key_usage().unwrap().unwrap().value;
    assert!(ku.digital_signature() && !ku.key_cert_sign() && !ku.crl_sign());
    let eku = cert.extended_key_usage().unwrap().unwrap().value;
    assert!(eku.server_auth && !eku.any && !eku.client_auth && !eku.code_signing);

    // Names: exactly the set.
    let info = leaf::inspect(der).unwrap();
    let mut names = info.dns_names.clone();
    names.sort();
    let mut want: Vec<String> = DNS_NAMES.iter().map(|s| (*s).to_owned()).collect();
    want.sort();
    assert_eq!(names, want);
    let mut ips = info.ip_addresses.clone();
    ips.sort();
    let mut want: Vec<IpAddr> = IP_ADDRESSES.to_vec();
    want.sort();
    assert_eq!(ips, want);

    // The marker: one raw byte, the .NET 10 version, not critical.
    assert_eq!(info.marker_version, Some(leaf::MARKER_VERSION));
    let marker = cert
        .extensions()
        .iter()
        .find(|e| e.oid.iter().is_some_and(|a| a.eq(ASPNET_MARKER_OID)))
        .unwrap();
    assert!(!marker.critical);
    assert_eq!(marker.value, [6]);

    // Validity: a day before now to 365 days after; self-signed.
    let secs = |t: SystemTime| {
        i64::try_from(t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()).unwrap()
    };
    assert_eq!(info.not_before, secs(now() - DAY));
    assert_eq!(info.not_after, secs(now() + DAY * 365));
    assert_eq!(cert.issuer(), cert.subject());
    leaf::check_trust_shape(der).unwrap();
}

#[test]
fn the_leaf_is_made_once_kept_and_trusted_once() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);

    let first = m.ensure(now()).unwrap();
    assert_eq!(first.status, DevCertStatus::Trusted);
    assert_eq!(first.material.origin, Origin::Generated);
    assert_eq!(fake.trust_calls(), 1);
    assert_eq!(m.status(now()), DevCertStatus::Trusted);

    let second = m.ensure(now() + DAY).unwrap();
    assert_eq!(second.status, DevCertStatus::Trusted);
    assert_eq!(second.material.cert_der(), first.material.cert_der());
    assert_eq!(
        second.material.key().pkcs8_der(),
        first.material.key().pkcs8_der()
    );
    assert_eq!(
        fake.trust_calls(),
        1,
        "already trusted: no second confirmation"
    );
}

#[test]
fn the_leaf_is_replaced_when_thirty_days_are_left_not_before() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);
    let first = m.ensure(now()).unwrap();

    // 365 days of validity: at day 335 exactly 30 days remain, which is still good.
    let keep = m.ensure(now() + DAY * 335).unwrap();
    assert_eq!(keep.material.cert_der(), first.material.cert_der());

    // One second later it has less than 30 days left: a new leaf, trusted again.
    let renewed = m
        .ensure(now() + DAY * 335 + Duration::from_secs(1))
        .unwrap();
    assert_ne!(renewed.material.cert_der(), first.material.cert_der());
    assert_ne!(
        renewed.material.key().pkcs8_der(),
        first.material.key().pkcs8_der()
    );
    assert_eq!(renewed.status, DevCertStatus::Trusted);
    assert_eq!(fake.trust_calls(), 2);
    // The new leaf is on disk: the next run keeps it.
    let again = m
        .ensure(now() + DAY * 335 + Duration::from_secs(2))
        .unwrap();
    assert_eq!(again.material.cert_der(), renewed.material.cert_der());
}

#[test]
fn a_leaf_that_has_not_begun_or_is_not_ours_is_not_used() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);
    let first = m.ensure(now()).unwrap();

    // The clock went back two days: the leaf starts "in the future", so it is replaced.
    let earlier = m.ensure(now() - DAY * 2).unwrap();
    assert_ne!(earlier.material.cert_der(), first.material.cert_der());

    // A file holding a CA certificate and some key is never used or offered for trust.
    let ca = Spec {
        ca: true,
        ..Spec::good()
    }
    .build();
    let body = format!(
        "{}{}",
        pem::encode(&pem::Pem::new("CERTIFICATE", ca.der.clone())),
        PrivateKey::from_pkcs8_der(b"junk".to_vec())
            .to_pem()
            .as_str()
    );
    puddle_fs::private::write_atomic(&leaf_path(&dir), body.as_bytes()).unwrap();
    let fake = FakeTrust::new(vec![]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    leaf::check_trust_shape(report.material.cert_der()).unwrap();
    assert!(
        fake.trust_calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| *c != ca.der),
        "a CA is never offered for trust"
    );
}

#[test]
fn garbage_in_the_leaf_file_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    puddle_fs::private::write_atomic(&leaf_path(&dir), b"not a pem file").unwrap();
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::Trusted);
    let on_disk = fs::read_to_string(leaf_path(&dir)).unwrap();
    assert!(on_disk.contains("BEGIN CERTIFICATE") && on_disk.contains("BEGIN PRIVATE KEY"));
}

#[test]
fn the_key_file_is_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    manager(&dir, &fake).ensure(now()).unwrap();
    let file = fs::File::open(leaf_path(&dir)).unwrap();
    puddle_fs::private::check(&file).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        let folder = leaf_path(&dir).parent().unwrap().to_owned();
        assert_eq!(
            fs::metadata(folder).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[cfg(unix)]
#[test]
fn a_leaf_file_others_can_read_is_replaced_never_used() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);
    let first = m.ensure(now()).unwrap();
    fs::set_permissions(leaf_path(&dir), fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(
        m.status(now()),
        DevCertStatus::NeedsTrust,
        "the exposed file is not read, so there is no trusted leaf"
    );
    let second = m.ensure(now()).unwrap();
    assert_ne!(
        second.material.key().pkcs8_der(),
        first.material.key().pkcs8_der(),
        "a leaked key is retired"
    );
    let mode = fs::metadata(leaf_path(&dir)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

// ---- what trust can end in ----------------------------------------------------------------------

#[test]
fn a_declined_confirmation_is_a_reported_state_and_the_guests_still_get_the_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]).answering(Ok(TrustOutcome::Declined));
    let m = manager(&dir, &fake);
    let report = m.ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::TrustDeclined);
    assert_eq!(report.material.origin, Origin::Generated);
    assert!(leaf_path(&dir).exists());
    assert_eq!(m.status(now()), DevCertStatus::TrustDeclined);

    // Asking again after the user changed their mind works: same leaf, now trusted.
    *fake.answer.lock().unwrap() = Ok(TrustOutcome::Trusted);
    let again = m.ensure(now()).unwrap();
    assert_eq!(again.status, DevCertStatus::Trusted);
    assert_eq!(again.material.cert_der(), report.material.cert_der());
    assert_eq!(m.status(now()), DevCertStatus::Trusted);
}

#[test]
fn a_failed_trust_is_a_reported_state_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]).answering(Err(TrustError::Failed(
        "the store refused the certificate: os error 0x5".into(),
    )));
    let m = manager(&dir, &fake);
    let report = m.ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::TrustFailed);
    assert!(report.detail.as_deref().unwrap().contains("0x5"));
    assert_eq!(m.status(now()), DevCertStatus::TrustFailed);
}

#[test]
fn a_host_without_trust_still_gets_the_leaf() {
    let dir = tempfile::tempdir().unwrap();
    let m = DevCertManager::new(dir.path(), Unsupported);
    assert_eq!(m.status(now()), DevCertStatus::Unsupported);
    let report = m.ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::Unsupported);
    assert_eq!(report.material.origin, Origin::Generated);
    assert!(leaf_path(&dir).exists());
    leaf::check_trust_shape(report.material.cert_der()).unwrap();
}

// ---- secrets --------------------------------------------------------------------------------------

#[test]
fn no_key_bytes_reach_debug_output_or_errors() {
    use std::fmt::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let fake = FakeTrust::new(vec![]);
    let m = manager(&dir, &fake);
    let report = m.ensure(now()).unwrap();
    let key = report.material.key().pkcs8_der().to_vec();
    let pem_key = report.material.key().to_pem();
    let b64 = pem_key
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<String>();
    let hex = key.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });

    let mut seen = format!("{report:?} {m:?} {:?}", report.material.key());
    // A failure path: the folder is a file, so writing fails.
    let blocked = tempfile::tempdir().unwrap();
    fs::write(blocked.path().join(puddle_certs::devcert::FOLDER), b"x").unwrap();
    let err = manager(&blocked, &fake).ensure(now()).unwrap_err();
    assert!(matches!(err, DevCertError::Folder { .. }));
    write!(seen, " {err} {err:?}").unwrap();

    for needle in [&b64, &hex] {
        assert!(!seen.contains(needle.as_str()), "key bytes in debug output");
    }
    assert!(!seen.contains("PRIVATE KEY"));
    assert!(format!("{:?}", report.material.key()).contains("PrivateKey"));

    // The same for a reused certificate's exported key.
    let fake = FakeTrust::new(vec![Spec::good().build()]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    let seen = format!("{report:?}");
    assert!(!seen.contains("fake-existing-key-bytes"));
    assert!(!seen.contains(&format!("{:?}", b"fake-existing-key-bytes".to_vec())));
}

#[test]
fn the_error_of_a_failing_export_names_the_cause_never_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let cert = Spec::good().build();
    let fake = FakeTrust::new(vec![cert]);
    fake.exported.lock().unwrap()[0].1 = None;
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    let text = report.skipped[0].reason.to_string();
    assert!(text.contains("exporting the key failed") && text.contains("0x8009000b"));
}

// ---- the shape gate ---------------------------------------------------------------------------

#[test]
fn only_a_marked_loopback_leaf_passes_the_trust_shape_check() {
    use puddle_certs::devcert::leaf::{ShapeError, check_trust_shape};
    let shape = |spec: Spec| check_trust_shape(&spec.build().der).map(|_| ());

    assert_eq!(shape(Spec::good()), Ok(()));
    assert_eq!(
        shape(Spec {
            ca: true,
            ..Spec::good()
        }),
        Err(ShapeError::NotALeaf)
    );
    assert_eq!(
        shape(Spec {
            marker: None,
            ..Spec::good()
        }),
        Err(ShapeError::NoMarker)
    );
    assert_eq!(
        shape(Spec {
            names: vec!["localhost", "login.example.com"],
            ..Spec::good()
        }),
        Err(ShapeError::ForeignName("login.example.com".into()))
    );
    assert_eq!(
        shape(Spec {
            names: vec![],
            ..Spec::good()
        }),
        Err(ShapeError::ForeignName("(none)".into()))
    );

    // A certificate that does not say it is not a CA is not trusted either, and neither is one
    // that is not a certificate.
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::NoCa;
    params.subject_alt_names = vec![SanType::DnsName("localhost".try_into().unwrap())];
    params.custom_extensions = vec![CustomExtension::from_oid_content(
        &ASPNET_MARKER_OID,
        vec![6],
    )];
    let der = params
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap()
        .der()
        .to_vec();
    assert_eq!(check_trust_shape(&der), Err(ShapeError::NotALeaf));
    assert_eq!(check_trust_shape(b"nope"), Err(ShapeError::Unreadable));
    let mut trailing = Spec::good().build().der;
    trailing.push(0);
    assert_eq!(check_trust_shape(&trailing), Err(ShapeError::Unreadable));
}

#[test]
fn a_leaf_for_a_foreign_name_is_never_offered_to_the_system() {
    // The file in the data folder is replaced by one that passes every other check but names a real
    // domain: it is not used, so it never reaches the trust call.
    let dir = tempfile::tempdir().unwrap();
    let bad = Spec {
        names: vec!["localhost", "bank.example"],
        ..Spec::good()
    }
    .build();
    let body = format!(
        "{}{}",
        pem::encode(&pem::Pem::new("CERTIFICATE", bad.der.clone())),
        PrivateKey::from_pkcs8_der(b"junk".to_vec())
            .to_pem()
            .as_str()
    );
    puddle_fs::private::write_atomic(&leaf_path(&dir), body.as_bytes()).unwrap();
    let fake = FakeTrust::new(vec![]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_ne!(report.material.cert_der(), bad.der);
    assert!(
        fake.trust_calls
            .lock()
            .unwrap()
            .iter()
            .all(|c| *c != bad.der)
    );
    leaf::check_trust_shape(report.material.cert_der()).unwrap();
}

// ---- failing hosts, odd certificates -------------------------------------------------------------

/// A host whose answers are set per test; the key and the existing certificates are never used.
struct Scripted {
    existing: Result<Vec<ExistingCert>, TrustError>,
    trusted: Result<bool, TrustError>,
    trust: Result<TrustOutcome, TrustError>,
}

impl DevCertTrust for Scripted {
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
        self.existing.clone()
    }
    fn export_key(&self, _: &[u8]) -> Result<PrivateKey, TrustError> {
        Err(TrustError::Failed("not scripted".into()))
    }
    fn is_trusted(&self, _: &[u8]) -> Result<bool, TrustError> {
        self.trusted.clone()
    }
    fn trust(&self, _: &[u8]) -> Result<TrustOutcome, TrustError> {
        self.trust.clone()
    }
}

#[test]
fn a_store_that_cannot_be_read_is_reported_and_the_leaf_is_still_made() {
    let dir = tempfile::tempdir().unwrap();
    let m = DevCertManager::new(
        dir.path(),
        Scripted {
            existing: Err(TrustError::Failed(
                "opening the My store: os error 0x5".into(),
            )),
            trusted: Ok(true),
            trust: Ok(TrustOutcome::Trusted),
        },
    );
    let report = m.ensure(now()).unwrap();
    assert_eq!(report.status, DevCertStatus::Trusted);
    assert!(report.detail.as_deref().unwrap().contains("0x5"));
    assert_eq!(report.material.origin, Origin::Generated);
    assert_eq!(m.status(now()), DevCertStatus::Trusted);
}

#[test]
fn every_way_the_trust_lookup_or_request_fails_is_a_state() {
    let ensure = |trusted: Result<bool, TrustError>, trust: Result<TrustOutcome, TrustError>| {
        let dir = tempfile::tempdir().unwrap();
        let m = DevCertManager::new(
            dir.path(),
            Scripted {
                existing: Ok(vec![]),
                trusted,
                trust,
            },
        );
        let report = m.ensure(now()).unwrap();
        (report.status, report.detail)
    };
    let boom = || TrustError::Failed("os error 0x5".into());
    assert_eq!(
        ensure(Err(TrustError::Unsupported), Ok(TrustOutcome::Trusted)).0,
        DevCertStatus::Unsupported
    );
    assert_eq!(
        ensure(Err(boom()), Ok(TrustOutcome::Trusted)),
        (DevCertStatus::TrustFailed, Some("os error 0x5".into()))
    );
    assert_eq!(
        ensure(Ok(false), Err(TrustError::Unsupported)).0,
        DevCertStatus::Unsupported
    );
    assert_eq!(
        ensure(Ok(false), Err(boom())),
        (DevCertStatus::TrustFailed, Some("os error 0x5".into()))
    );
}

#[test]
fn a_certificate_that_is_not_readable_is_ignored_and_other_names_are_not_ours() {
    let dir = tempfile::tempdir().unwrap();
    let junk = ExistingCert {
        der: b"junk".to_vec(),
        key: KeyAccess::Exportable,
        trusted: true,
    };
    let fake = FakeTrust::new(vec![junk]);
    let report = manager(&dir, &fake).ensure(now()).unwrap();
    assert_eq!(report.material.origin, Origin::Generated);
    assert_eq!(report.skipped.len(), 0);

    // An e-mail name in the SAN is neither a DNS name nor an address: it is left out of both lists,
    // and a foreign address is refused by the shape check.
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::ExplicitNoCa;
    params.subject_alt_names = vec![
        SanType::Rfc822Name("dev@example.com".try_into().unwrap()),
        SanType::IpAddress("10.1.2.3".parse().unwrap()),
    ];
    params.custom_extensions = vec![CustomExtension::from_oid_content(
        &ASPNET_MARKER_OID,
        vec![6],
    )];
    let der = params
        .self_signed(&KeyPair::generate().unwrap())
        .unwrap()
        .der()
        .to_vec();
    let info = leaf::inspect(&der).unwrap();
    assert_eq!(info.dns_names.len(), 0);
    assert_eq!(info.ip_addresses, ["10.1.2.3".parse::<IpAddr>().unwrap()]);
    assert_eq!(
        leaf::check_trust_shape(&der),
        Err(leaf::ShapeError::ForeignName("10.1.2.3".into()))
    );
}

#[test]
fn the_host_without_trust_says_so_for_every_call_and_the_material_converts_to_pem() {
    assert_eq!(Unsupported.existing(), Err(TrustError::Unsupported));
    assert_eq!(
        Unsupported.export_key(b"c").err(),
        Some(TrustError::Unsupported)
    );
    assert_eq!(Unsupported.is_trusted(b"c"), Err(TrustError::Unsupported));
    assert_eq!(Unsupported.trust(b"c"), Err(TrustError::Unsupported));

    let dir = tempfile::tempdir().unwrap();
    let report = DevCertManager::new(dir.path(), Unsupported)
        .ensure(now())
        .unwrap();
    let cert_pem = report.material.cert_pem();
    let parsed = pem::parse(&cert_pem).unwrap();
    assert_eq!(parsed.tag(), "CERTIFICATE");
    assert_eq!(parsed.contents(), report.material.cert_der());
    let key_pem = report.material.key().to_pem();
    let parsed = pem::parse(key_pem.as_str()).unwrap();
    assert_eq!(parsed.tag(), "PRIVATE KEY");
    assert_eq!(parsed.contents(), report.material.key().pkcs8_der());
}
