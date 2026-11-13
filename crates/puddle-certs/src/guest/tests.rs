// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::{Duration, UNIX_EPOCH};

use puddle_ca::{CaBuilder, GUEST_BUNDLE_PATH};

use super::*;
use crate::store::{SOURCES, StoreSnapshot};

fn root_der(cn: &str) -> Vec<u8> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, cn);
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.not_after = rcgen::date_time_ymd(2040, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    p.self_signed(&key).unwrap().der().to_vec()
}

fn corporate(ders: &[Vec<u8>]) -> CorporateRoots {
    let mut s = StoreSnapshot::new();
    for d in ders {
        s.add(SOURCES[0], d.clone());
    }
    CorporateRoots::select(&s, UNIX_EPOCH + Duration::from_hours(525_960))
}

fn puddle_ca() -> CaCertificate {
    CaBuilder::new("Puddle test CA")
        .build()
        .unwrap()
        .certificate()
        .clone()
}

fn text(files: &[GuestFile], p: &str) -> String {
    let f = files.iter().find(|f| f.path().as_str() == p).unwrap();
    String::from_utf8(f.contents().to_vec()).unwrap()
}

#[test]
fn host_roots_and_puddle_cas_become_files_env_and_a_step() {
    let corp = corporate(&[root_der("Corp Root"), root_der("User Root")]);
    let ca = puddle_ca();
    let trust = GuestTrust::new(&corp, &TrustBundle::new().with(ca.clone()));
    let files = trust.guest_files();
    let paths: Vec<&str> = files.iter().map(|f| f.path().as_str()).collect();

    let host_files: Vec<&str> = paths
        .iter()
        .copied()
        .filter(|p| p.starts_with(HOST_CA_DIR))
        .collect();
    let expected: Vec<String> = corp
            .certificates()
            .iter()
            // flake-ok: a path inside the guest, which is Linux whatever the host is
            .map(|c| format!("{HOST_CA_DIR}/{}.crt", &c.fingerprint().to_string()[..16]))
            .collect();
    assert_eq!(host_files, expected);
    assert_eq!(text(&files, &expected[0]), corp.certificates()[0].pem());
    assert_eq!(
        &paths[2..],
        [
            "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
            GUEST_BUNDLE_PATH,
            EXTRA_CAS_PATH,
            BUNDLE_STEP_PATH,
            JAVA_STEP_PATH,
        ]
    );
    let extra = text(&files, EXTRA_CAS_PATH);
    assert_eq!(extra.matches("-----BEGIN CERTIFICATE-----").count(), 3);
    assert!(extra.starts_with(&corp.certificates()[0].pem()));
    assert!(extra.ends_with(ca.pem()));
    let step = files
        .iter()
        .find(|f| f.path().as_str() == BUNDLE_STEP_PATH)
        .unwrap();
    assert_eq!(step.mode(), 0o755);
    assert_eq!(step.contents(), BUNDLE_STEP_SH.as_bytes());
    let java = files
        .iter()
        .find(|f| f.path().as_str() == JAVA_STEP_PATH)
        .unwrap();
    assert_eq!(java.mode(), 0o755);
    assert_eq!(java.contents(), JAVA_STEP_SH.as_bytes());
    assert!(
        files
            .iter()
            .filter(|f| ![BUNDLE_STEP_PATH, JAVA_STEP_PATH].contains(&f.path().as_str()))
            .all(|f| f.mode() == 0o644)
    );

    let env = trust.env();
    // Tools that add to their built-in roots take the extra CAs only.
    for name in ["NODE_EXTRA_CA_CERTS", "DENO_CERT"] {
        assert_eq!(env.get(name), Some(EXTRA_CAS_PATH), "{name}");
    }
    // Tools that replace their roots take the full bundle.
    for name in [
        "SSL_CERT_FILE",
        "REQUESTS_CA_BUNDLE",
        "CURL_CA_BUNDLE",
        "PIP_CERT",
        "GIT_SSL_CAINFO",
    ] {
        assert_eq!(env.get(name), Some(CA_BUNDLE_PATH), "{name}");
    }
    assert_eq!(env.len(), 7);
    assert_eq!(
        trust.boot_steps(),
        [guest_path!(BUNDLE_STEP_PATH), guest_path!(JAVA_STEP_PATH)]
    );
    assert!(!trust.is_empty());
}

#[test]
fn only_puddle_cas_or_only_host_roots_still_sync() {
    let only_ca = GuestTrust::new(
        &CorporateRoots::default(),
        &TrustBundle::new().with(puddle_ca()),
    );
    assert_eq!(only_ca.boot_steps().len(), 2);
    assert_eq!(
        text(&only_ca.guest_files(), EXTRA_CAS_PATH)
            .matches("BEGIN")
            .count(),
        1
    );
    let only_host = GuestTrust::new(&corporate(&[root_der("Corp")]), &TrustBundle::new());
    let files = only_host.guest_files();
    assert_eq!(files.len(), 4, "one root, extra-cas, two steps");
    assert!(!only_host.env().is_empty());
}

#[test]
fn nothing_to_add_leaves_the_guest_alone() {
    let trust = GuestTrust::new(&CorporateRoots::default(), &TrustBundle::new());
    assert!(trust.is_empty());
    assert_eq!(trust.guest_files(), []);
    assert_eq!(trust.env(), GuestEnv::new());
    assert_eq!(trust.boot_steps(), []);
}

#[test]
fn unchanged_inputs_give_byte_identical_files() {
    let ders = [root_der("A"), root_der("B")];
    let ca = puddle_ca();
    let one = GuestTrust::new(&corporate(&ders), &TrustBundle::new().with(ca.clone()));
    let reversed = [ders[1].clone(), ders[0].clone()];
    let two = GuestTrust::new(&corporate(&reversed), &TrustBundle::new().with(ca));
    assert_eq!(one.guest_files(), two.guest_files());
    assert_eq!(one.env(), two.env());
}

#[test]
fn a_ca_present_on_both_sides_is_listed_once() {
    let ca = puddle_ca();
    // The user also trusted Puddle's CA in Windows (a dev CA).
    let corp = corporate(&[ca.der().to_vec(), root_der("Corp")]);
    let trust = GuestTrust::new(&corp, &TrustBundle::new().with(ca));
    let extra = text(&trust.guest_files(), EXTRA_CAS_PATH);
    assert_eq!(extra.matches("-----BEGIN CERTIFICATE-----").count(), 2);
}

#[test]
fn a_development_certificate_the_user_also_trusted_in_windows_is_listed_once() {
    let dev = dev_cert();
    let der = pem::parse(dev.cert_pem()).unwrap().into_contents();
    // The same certificate among the host's roots (a leaf is no root `select` would take, so
    // the host list is built directly).
    let trust = GuestTrust {
        host: vec![(Fingerprint::of(&der), dev.cert_pem().to_owned())],
        puddle: TrustBundle::new(),
        dev: Some(dev),
        user: SessionUser::Root,
    };
    let extra = text(&trust.guest_files(), EXTRA_CAS_PATH);
    assert_eq!(extra.matches("-----BEGIN CERTIFICATE-----").count(), 1);
}

fn dev_cert() -> GuestDevCert {
    let dir = tempfile::tempdir().unwrap();
    let manager = crate::devcert::DevCertManager::new(dir.path(), crate::devcert::Unsupported);
    let report = manager.ensure(std::time::SystemTime::now()).unwrap();
    GuestDevCert::new(&report.material)
}

#[test]
fn a_development_certificate_alone_is_a_trust_with_files_env_and_two_steps() {
    let dev = dev_cert();
    let trust =
        GuestTrust::new(&CorporateRoots::default(), &TrustBundle::new()).with_dev_cert(dev.clone());
    assert!(!trust.is_empty());
    let files = trust.guest_files();
    let paths: Vec<&str> = files.iter().map(|f| f.path().as_str()).collect();
    assert_eq!(
        paths,
        [
            "/usr/local/share/ca-certificates/puddle-dev/localhost.crt",
            EXTRA_CAS_PATH,
            BUNDLE_STEP_PATH,
            JAVA_STEP_PATH,
            "/etc/puddle/dev-cert/localhost.crt",
            "/etc/puddle/dev-cert/localhost.key",
            "/usr/local/lib/puddle/dev-cert.sh",
        ]
    );
    let modes: Vec<u32> = files.iter().map(GuestFile::mode).collect();
    assert_eq!(modes, [0o644, 0o644, 0o755, 0o755, 0o644, 0o600, 0o755]);
    // The certificate is the trust anchor in the extra CAs, once.
    assert_eq!(text(&files, EXTRA_CAS_PATH), dev.cert_pem());
    assert!(
        text(&files, "/etc/puddle/dev-cert/localhost.key")
            .starts_with("-----BEGIN PRIVATE KEY-----")
    );
    let env = trust.env();
    assert_eq!(
        env.get("DEV_CERT_PEM"),
        Some("/etc/puddle/dev-cert/localhost.crt")
    );
    assert_eq!(
        env.get("DEV_KEY_PEM"),
        Some("/etc/puddle/dev-cert/localhost.key")
    );
    assert_eq!(env.get("DOTNET_GENERATE_ASPNET_CERTIFICATE"), Some("false"));
    assert!(
        env.get("SSL_CERT_DIR")
            .unwrap()
            .starts_with("/root/.aspnet/dev-certs/trust:")
    );
    let as_agent = trust.clone().for_user(SessionUser::Agent).env();
    assert_eq!(
        as_agent.get("SSL_CERT_DIR"),
        Some("/home/agent/.aspnet/dev-certs/trust:/etc/ssl/certs:/usr/lib/ssl/certs")
    );
    assert_eq!(env.get("SSL_CERT_FILE"), Some(CA_BUNDLE_PATH));
    let steps: Vec<String> = trust.boot_steps().iter().map(ToString::to_string).collect();
    assert_eq!(
        steps,
        [
            BUNDLE_STEP_PATH,
            JAVA_STEP_PATH,
            "/usr/local/lib/puddle/dev-cert.sh"
        ]
    );
}

#[test]
fn the_development_certificate_sits_after_the_other_extra_cas_and_is_not_repeated() {
    let dev = dev_cert();
    let corp = corporate(&[root_der("Corp")]);
    let ca = puddle_ca();
    let trust =
        GuestTrust::new(&corp, &TrustBundle::new().with(ca.clone())).with_dev_cert(dev.clone());
    let extra = text(&trust.guest_files(), EXTRA_CAS_PATH);
    assert_eq!(extra.matches("-----BEGIN CERTIFICATE-----").count(), 3);
    assert!(
        extra.ends_with(dev.cert_pem()),
        "the dev certificate is last"
    );
    // The same bytes every time: the boot hook sees no change.
    let again = GuestTrust::new(&corp, &TrustBundle::new().with(ca)).with_dev_cert(dev);
    assert_eq!(again.guest_files(), trust.guest_files());
}

#[test]
fn no_key_byte_reaches_debug_output() {
    let dev = dev_cert();
    let key_body: String = dev
        .guest_files()
        .iter()
        .find(|f| f.path().as_str() == "/etc/puddle/dev-cert/localhost.key")
        .map(|f| String::from_utf8(f.contents().to_vec()).unwrap())
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_owned();
    let trust =
        GuestTrust::new(&CorporateRoots::default(), &TrustBundle::new()).with_dev_cert(dev.clone());
    let bytes: Vec<u8> = key_body.bytes().collect();
    assert_eq!(format!("{dev:?}"), "GuestDevCert { .. }");
    let leaked = format!("{dev:?} {trust:?} {:?}", trust.guest_files());
    assert!(!leaked.contains(&key_body), "key text in Debug output");
    assert!(
        !leaked.contains(format!("{:?}", &bytes[..16]).trim_matches(['[', ']'])),
        "key bytes in Debug output"
    );
}
