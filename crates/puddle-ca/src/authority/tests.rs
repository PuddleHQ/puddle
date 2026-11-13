// SPDX-License-Identifier: AGPL-3.0-or-later
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

use super::*;

const HOUR: Duration = Duration::from_secs(3600);

fn ca() -> WorkspaceCa {
    CaBuilder::new("Puddle proxy CA (workspace test)")
        .build()
        .unwrap()
}

fn unix(t: OffsetDateTime) -> UnixTime {
    UnixTime::since_unix_epoch(Duration::from_secs(
        u64::try_from(t.unix_timestamp()).unwrap(),
    ))
}

/// Verifies `leaf` as a TLS server certificate for `host` under `anchor`, as rustls does.
fn verify(
    anchor: &CaCertificate,
    leaf: &CertificateDer<'_>,
    host: &str,
    at: OffsetDateTime,
) -> Result<(), webpki::Error> {
    let anchors = [webpki::anchor_from_trusted_cert(anchor.der())?];
    let ee = webpki::EndEntityCert::try_from(leaf)?;
    ee.verify_for_usage(
        webpki::ALL_VERIFICATION_ALGS,
        &anchors,
        &[],
        unix(at),
        webpki::KeyUsage::server_auth(),
        None,
        None,
    )?;
    let name = ServerName::try_from(host).unwrap();
    ee.verify_is_valid_for_subject_name(&name)
}

#[test]
fn a_leaf_for_any_dns_name_verifies_under_the_ca() {
    let ca = ca();
    let now = OffsetDateTime::now_utc();
    for host in [
        "github.com",
        "api.github.com",
        "dev.azure.com",
        "GitHub.com.",
        "example.com",
        "evilgithub.com",
        "a.b.c.example.org",
    ] {
        let leaf = ca.leaf(host).unwrap();
        let expected = host.trim_end_matches('.').to_ascii_lowercase();
        verify(
            ca.certificate(),
            leaf.end_entity_cert().unwrap(),
            &expected,
            now,
        )
        .unwrap();
        leaf.keys_match().unwrap();
    }
}

#[test]
fn the_ca_refuses_what_is_not_a_dns_name() {
    let ca = ca();
    for bad in [
        "",
        "*.github.com",
        "git hub.com",
        "github.com:443",
        "140.82.112.3",
        "127.0.0.1",
        "::1",
        "[2606:50c0:8000::153]",
    ] {
        assert!(
            matches!(ca.leaf(bad), Err(CaError::InvalidHost { .. })),
            "{bad:?}"
        );
    }
    assert_eq!(ca.cached_leaves(), 0);
}

#[test]
fn a_leaf_from_one_workspace_does_not_verify_under_another() {
    let a = ca();
    let b = ca();
    assert_ne!(a.certificate(), b.certificate());
    let leaf = a.leaf("github.com").unwrap();
    let now = OffsetDateTime::now_utc();
    assert!(
        verify(
            b.certificate(),
            leaf.end_entity_cert().unwrap(),
            "github.com",
            now
        )
        .is_err()
    );
}

#[test]
fn leaves_are_cached_per_host_and_reissued_before_they_expire() {
    let ca = ca();
    let t0 = OffsetDateTime::now_utc();
    let first = ca.leaf_at("github.com", t0).unwrap();
    let again = ca
        .leaf_at("GITHUB.com", t0 + time::Duration::hours(1))
        .unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    let other = ca.leaf_at("api.github.com", t0).unwrap();
    assert!(!Arc::ptr_eq(&first, &other));

    // Three quarters into the seven-day default, the cache hands out a new leaf.
    let late = t0 + time::Duration::hours(7 * 24 * 3 / 4);
    let renewed = ca.leaf_at("github.com", late).unwrap();
    assert!(!Arc::ptr_eq(&first, &renewed));
    verify(
        ca.certificate(),
        renewed.end_entity_cert().unwrap(),
        "github.com",
        late + time::Duration::days(3),
    )
    .unwrap();
    assert_eq!(ca.cached_leaves(), 2);
}

#[test]
fn the_cache_clock_counts_up_by_one_per_use() {
    let mut cache = LeafCache::new(2);
    assert_eq!(cache.tick(), 1);
    assert_eq!(cache.tick(), 2);
    assert_eq!(cache.tick(), 3);
}

#[test]
fn the_cache_evicts_the_least_recently_used_leaf() {
    let ca = CaBuilder::new("Puddle test CA")
        .leaf_cache_capacity(2)
        .build()
        .unwrap();
    let now = OffsetDateTime::now_utc();
    let a = ca.leaf_at("a.github.com", now).unwrap();
    let b = ca.leaf_at("b.github.com", now).unwrap();
    // Touch a, so b is the least recently used when c arrives.
    assert!(Arc::ptr_eq(&a, &ca.leaf_at("a.github.com", now).unwrap()));
    ca.leaf_at("c.github.com", now).unwrap();
    assert_eq!(ca.cached_leaves(), 2);
    assert!(Arc::ptr_eq(&a, &ca.leaf_at("a.github.com", now).unwrap()));
    assert!(!Arc::ptr_eq(&b, &ca.leaf_at("b.github.com", now).unwrap()));
    assert_eq!(ca.cached_leaves(), 2);
}

#[test]
fn leaf_validity_is_clamped_to_the_ca() {
    let ca = CaBuilder::new("Puddle test CA")
        .ca_validity(10 * HOUR)
        .leaf_validity(10 * HOUR)
        .backdate(HOUR)
        .build()
        .unwrap();
    let t0 = OffsetDateTime::now_utc();
    let later = t0 + time::Duration::hours(5);
    let leaf = ca.leaf_at("github.com", later).unwrap();
    let der = leaf.end_entity_cert().unwrap();
    // Valid at the CA's last hour, not after the CA ends.
    verify(
        ca.certificate(),
        der,
        "github.com",
        t0 + time::Duration::hours(9),
    )
    .unwrap();
    assert!(
        verify(
            ca.certificate(),
            der,
            "github.com",
            t0 + time::Duration::hours(11)
        )
        .is_err()
    );
    assert!(matches!(
        ca.leaf_at("github.com", t0 + time::Duration::hours(11)),
        Err(CaError::Expired)
    ));
}

#[test]
fn backdating_covers_a_lagging_guest_clock() {
    let ca = ca();
    let now = OffsetDateTime::now_utc();
    let leaf = ca.leaf_at("github.com", now).unwrap();
    let lagging = now - time::Duration::hours(12);
    verify(
        ca.certificate(),
        leaf.end_entity_cert().unwrap(),
        "github.com",
        lagging,
    )
    .unwrap();
}

#[test]
fn builder_refuses_meaningless_settings() {
    let check = |builder: CaBuilder| builder.build().unwrap_err();
    for builder in [
        CaBuilder::new(" "),
        CaBuilder::new(&"n".repeat(65)),
        CaBuilder::new("x").ca_validity(Duration::ZERO),
        CaBuilder::new("x").leaf_validity(Duration::ZERO),
        CaBuilder::new("x")
            .ca_validity(HOUR)
            .leaf_validity(2 * HOUR),
        CaBuilder::new("x").leaf_cache_capacity(0),
        CaBuilder::new("x").ca_validity(Duration::MAX),
        CaBuilder::new("x").backdate(Duration::MAX),
    ] {
        assert!(matches!(check(builder), CaError::InvalidSetting { .. }));
    }
}

#[test]
fn the_defaults_are_a_year_a_week_a_day_and_256_leaves() {
    assert_eq!(CaBuilder::DEFAULT_CA_VALIDITY, 365 * 24 * HOUR);
    assert_eq!(CaBuilder::DEFAULT_LEAF_VALIDITY, 7 * 24 * HOUR);
    assert_eq!(CaBuilder::DEFAULT_BACKDATE, 24 * HOUR);
    assert_eq!(CaBuilder::DEFAULT_LEAF_CACHE_CAPACITY, 256);
    let now = OffsetDateTime::now_utc();
    let ca = CaBuilder::new("Puddle test CA").build_at(now).unwrap();
    assert_eq!(ca.not_after, now + time::Duration::days(365));
    assert_eq!(ca.not_before, now - time::Duration::days(1));
}

#[test]
fn a_common_name_of_64_bytes_is_the_longest_allowed() {
    assert!(CaBuilder::new(&"n".repeat(64)).build().is_ok());
    assert!(matches!(
        CaBuilder::new(&"n".repeat(65)).build(),
        Err(CaError::InvalidSetting { .. })
    ));
}

#[test]
fn debug_names_the_type_and_the_expiry_only() {
    let now = OffsetDateTime::now_utc();
    let ca = CaBuilder::new("Puddle test CA").build_at(now).unwrap();
    let debug = format!("{ca:?}");
    assert!(debug.starts_with("WorkspaceCa {"), "{debug}");
    assert!(debug.contains("not_after"), "{debug}");
    assert!(debug.contains(&ca.not_after.year().to_string()), "{debug}");
}

#[test]
fn a_leaf_is_kept_until_exactly_three_quarters_of_its_life() {
    let ca = ca();
    let t0 = OffsetDateTime::now_utc();
    let first = ca.leaf_at("github.com", t0).unwrap();
    let three_quarters = 7 * 24 * 3600 * 3 / 4;
    let before = t0 + time::Duration::seconds(three_quarters - 1);
    assert!(Arc::ptr_eq(
        &first,
        &ca.leaf_at("github.com", before).unwrap()
    ));
    let at = t0 + time::Duration::seconds(three_quarters);
    assert!(!Arc::ptr_eq(&first, &ca.leaf_at("github.com", at).unwrap()));
}

#[test]
fn the_cache_evicts_in_use_order_over_several_rounds() {
    let ca = CaBuilder::new("Puddle test CA")
        .leaf_cache_capacity(3)
        .build()
        .unwrap();
    let now = OffsetDateTime::now_utc();
    let host = |n: u32| format!("h{n}.example.com");
    let leaves: Vec<_> = (0..3).map(|n| ca.leaf_at(&host(n), now).unwrap()).collect();
    // Use order is now 1, 2, 0 (oldest first): 1 is the next to go.
    ca.leaf_at(&host(2), now).unwrap();
    ca.leaf_at(&host(0), now).unwrap();
    ca.leaf_at(&host(3), now).unwrap();
    assert!(Arc::ptr_eq(&leaves[0], &ca.leaf_at(&host(0), now).unwrap()));
    assert!(Arc::ptr_eq(&leaves[2], &ca.leaf_at(&host(2), now).unwrap()));
    // Order is 3, 0, 2 now; 1 was evicted, so asking for it evicts 3.
    assert!(!Arc::ptr_eq(
        &leaves[1],
        &ca.leaf_at(&host(1), now).unwrap()
    ));
    assert!(Arc::ptr_eq(&leaves[0], &ca.leaf_at(&host(0), now).unwrap()));
    assert!(Arc::ptr_eq(&leaves[2], &ca.leaf_at(&host(2), now).unwrap()));
    assert_eq!(ca.cached_leaves(), 3);
}

/// DER of an extension whose OID is `2.5.29.<last>`: `OBJECT IDENTIFIER` (06 03 55 1d <last>).
fn oid(last: u8) -> [u8; 5] {
    [0x06, 0x03, 0x55, 0x1d, last]
}

#[test]
fn the_ca_certificate_is_a_critical_ca_with_path_length_0_and_no_name_constraint() {
    // Checked in the DER: rustls-webpki reads the name constraints of a trust anchor but not
    // its path length, so a webpki chain test could not tell `pathLen 0` from none. OpenSSL,
    // Go and Node enforce it.
    let ca = ca();
    let der = ca.certificate().der().as_ref();
    // basicConstraints, critical: `01 01 ff`, then the value `SEQUENCE { TRUE, INTEGER 0 }`.
    let basic: Vec<u8> = oid(0x13)
        .into_iter()
        .chain([
            0x01, 0x01, 0xff, 0x04, 0x08, 0x30, 0x06, 0x01, 0x01, 0xff, 0x02, 0x01, 0x00,
        ])
        .collect();
    assert!(der.windows(basic.len()).any(|w| w == basic.as_slice()));
    // nameConstraints (2.5.29.30) is nowhere in the certificate.
    let names = oid(0x1e);
    assert!(!der.windows(names.len()).any(|w| w == names));
    assert!(
        ca.certificate()
            .pem()
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
}

#[test]
fn no_key_material_leaves_the_ca() {
    let ca = ca();
    // The private scalar inside the PKCS#8 `ECPrivateKey`: `version 1, OCTET STRING (32)`.
    let key = ca.key_der_for_test();
    let marker = [0x02, 0x01, 0x01, 0x04, 0x20];
    let at = key.windows(marker.len()).position(|w| w == marker).unwrap() + marker.len();
    let needle = &key[at..at + 32];
    let leaf = ca.leaf("github.com").unwrap();
    let bundle = crate::TrustBundle::new().with(ca.certificate().clone());
    let mut seen: Vec<Vec<u8>> = vec![
        format!("{ca:?}").into_bytes(),
        format!("{:?}", ca.certificate()).into_bytes(),
        ca.certificate().pem().as_bytes().to_vec(),
        ca.certificate().der().to_vec(),
        format!("{:?}", ca.leaf("1.2.3.4").unwrap_err()).into_bytes(),
        format!("{leaf:?}").into_bytes(),
    ];
    seen.extend(leaf.cert.iter().map(|c| c.to_vec()));
    seen.extend(bundle.guest_files().iter().map(|f| f.contents().to_vec()));
    for bytes in &seen {
        assert!(!bytes.windows(needle.len()).any(|w| w == needle));
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("PRIVATE KEY"));
    }
    let debug = format!("{ca:?}");
    assert!(
        !debug.contains("issuer") && !debug.contains("cache"),
        "{debug}"
    );
}

// Autoref probe: `Probe::<T>::IMPLS` is true only when the inherent impl's bound holds.
struct Probe<T>(std::marker::PhantomData<T>);
trait NotImplemented {
    const IMPLS: bool = false;
}
impl<T> NotImplemented for Probe<T> {}
struct CloneProbe<T>(std::marker::PhantomData<T>);
impl<T> NotImplemented for CloneProbe<T> {}
impl<T: serde::Serialize> Probe<T> {
    const IMPLS: bool = true;
}
impl<T: Clone> CloneProbe<T> {
    const IMPLS: bool = true;
}

#[test]
fn the_ca_can_be_neither_serialised_nor_cloned() {
    const { assert!(!Probe::<WorkspaceCa>::IMPLS) };
    const { assert!(!CloneProbe::<WorkspaceCa>::IMPLS) };
    // The probes do see the traits where they exist.
    const { assert!(Probe::<String>::IMPLS) };
    const { assert!(CloneProbe::<CaCertificate>::IMPLS) };
}
