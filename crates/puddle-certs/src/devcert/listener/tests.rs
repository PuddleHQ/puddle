// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn names(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn the_leaf_names_the_interface_and_the_public_hosts_and_cannot_sign() {
    let dir = tempfile::tempdir().unwrap();
    let cert = ensure(
        dir.path(),
        &[ip("192.168.1.10")],
        &names(&["puddle.example.org"]),
        SystemTime::now(),
    )
    .unwrap();
    let info = leaf::inspect(cert.cert_der()).unwrap();
    assert_eq!(info.dns_names, ["puddle.example.org"]);
    assert_eq!(info.ip_addresses, [ip("192.168.1.10")]);
    assert_eq!(info.basic_constraints, Some((true, false)));
    assert!(!info.is_ca());
    assert!(cert.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));
    assert_eq!(cert.fingerprint().to_string().len(), 64);
}

#[test]
fn the_same_names_reuse_the_stored_leaf_and_other_names_replace_it() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let first = ensure(dir.path(), &[ip("10.0.0.5")], &names(&["a.example"]), now).unwrap();
    let again = ensure(dir.path(), &[ip("10.0.0.5")], &names(&["a.example"]), now).unwrap();
    assert_eq!(first.cert_der(), again.cert_der());
    assert_eq!(first.key().pkcs8_der(), again.key().pkcs8_der());
    let other = ensure(dir.path(), &[ip("10.0.0.6")], &names(&["a.example"]), now).unwrap();
    assert_ne!(first.cert_der(), other.cert_der());
    let renamed = ensure(dir.path(), &[ip("10.0.0.6")], &names(&["b.example"]), now).unwrap();
    assert_ne!(other.cert_der(), renamed.cert_der());
}

#[test]
fn a_leaf_close_to_expiry_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let first = ensure(dir.path(), &[ip("10.0.0.5")], &[], now).unwrap();
    let later = now + VALIDITY - Duration::from_hours(24);
    let renewed = ensure(dir.path(), &[ip("10.0.0.5")], &[], later).unwrap();
    assert_ne!(first.cert_der(), renewed.cert_der());
}

#[cfg(unix)]
#[test]
fn a_key_file_others_can_read_is_never_used() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let first = ensure(dir.path(), &[ip("10.0.0.5")], &[], now).unwrap();
    let file = dir.path().join(FOLDER).join(FILE);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    let second = ensure(dir.path(), &[ip("10.0.0.5")], &[], now).unwrap();
    assert_ne!(first.cert_der(), second.cert_der());
}

#[test]
fn the_certificate_alone_is_written_for_the_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let cert = ensure(dir.path(), &[ip("10.0.0.5")], &[], now).unwrap();
    let file = dir.path().join(FOLDER).join(CERT_FILE);
    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(text, cert.cert_pem());
    assert!(!text.contains("PRIVATE KEY"));
    // Rewritten when the stored leaf is reused.
    fs::remove_file(&file).unwrap();
    ensure(dir.path(), &[ip("10.0.0.5")], &[], now).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), cert.cert_pem());
}
