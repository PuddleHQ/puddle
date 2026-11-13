// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::CaBuilder;

fn cert(name: &str) -> CaCertificate {
    CaBuilder::new(name).build().unwrap().certificate().clone()
}

#[test]
fn empty_bundle_writes_nothing() {
    assert_eq!(TrustBundle::new().guest_files(), [] as [GuestFile; 0]);
}

#[test]
fn the_ca_text_and_every_guest_file_have_lf_line_ends_on_any_host() {
    let bundle = TrustBundle::new()
        .with(cert("Puddle proxy CA"))
        .with(cert("Puddle second CA"));
    for ca in bundle.certificates() {
        let text = ca.pem();
        assert!(
            text.starts_with("-----BEGIN CERTIFICATE-----\n"),
            "a CA text does not start with the BEGIN line"
        );
        assert!(
            text.ends_with("-----END CERTIFICATE-----\n"),
            "a CA text does not end with the END line and a line feed"
        );
        assert!(!text.contains('\r'), "a CA text holds a carriage return");
        assert_eq!(pem::parse(text).unwrap().contents(), ca.der().as_ref());
    }
    let files = bundle.guest_files();
    assert_eq!(files.len(), 3);
    let carriage_returns: Vec<_> = files
        .iter()
        .map(|f| (f.path().as_str(), f.contents().contains(&b'\r')))
        .collect();
    assert!(
        carriage_returns.iter().all(|(_, has)| !has),
        "{carriage_returns:?}"
    );
}

#[test]
fn each_ca_gets_its_own_file_and_all_share_the_bundle_file() {
    let proxy = cert("Puddle proxy CA");
    let dev = cert("Puddle second CA");
    let bundle = TrustBundle::new()
        .with(proxy.clone())
        .with(dev.clone())
        .with(proxy.clone());
    assert_eq!(bundle.certificates(), [proxy.clone(), dev.clone()]);

    let files = bundle.guest_files();
    let paths: Vec<&str> = files.iter().map(|f| f.path().as_str()).collect();
    assert_eq!(
        paths,
        [
            "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
            "/usr/local/share/ca-certificates/puddle/puddle-ca-2.crt",
            GUEST_BUNDLE_PATH,
        ]
    );
    assert_eq!(files[0].contents(), proxy.pem().as_bytes());
    assert_eq!(files[1].contents(), dev.pem().as_bytes());
    assert_eq!(
        files[2].contents(),
        format!("{}{}", proxy.pem(), dev.pem()).as_bytes()
    );
    assert!(files.iter().all(|f| f.mode() == GuestFile::DEFAULT_MODE));
    let text = String::from_utf8(files[2].contents().to_vec()).unwrap();
    assert_eq!(text.matches("-----BEGIN CERTIFICATE-----").count(), 2);
}
