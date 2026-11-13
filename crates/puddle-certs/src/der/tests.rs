// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use proptest::prelude::*;

fn cert(cn: &str, not_after: (i32, u8, u8)) -> Vec<u8> {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn);
    params
        .distinguished_name
        .push(rcgen::DnType::OrganizationName, "Example Corp");
    params.not_after = rcgen::date_time_ymd(not_after.0, not_after.1, not_after.2);
    let key = rcgen::KeyPair::generate().unwrap();
    params.self_signed(&key).unwrap().der().to_vec()
}

#[test]
fn reads_names_key_validity_and_common_name() {
    let der = cert("Example Corp Root CA", (2040, 6, 30));
    let f = parse(&der).unwrap();
    assert_eq!(f.issuer, f.subject, "self-signed");
    assert_eq!(f.subject.first(), Some(&SEQUENCE));
    assert_eq!(f.spki.first(), Some(&SEQUENCE));
    assert_eq!(f.subject_cn.as_deref(), Some("Example Corp Root CA"));
    // 2040-06-30T00:00:00Z
    assert_eq!(f.not_after, 2_224_627_200);
    // rcgen writes GeneralizedTime from 2050 on.
    let late = cert("Late", (2051, 1, 1));
    assert_eq!(parse(&late).unwrap().not_after, 2_556_143_999 + 1);
}

#[test]
fn civil_dates_match_known_epochs() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    assert_eq!(days_from_civil(1950, 1, 1), -7305);
    assert_eq!(days_from_civil(2024, 2, 29), 19_782);
}

#[test]
fn times_parse_in_both_forms_and_refuse_garbage() {
    let t = |tag: u8, s: &str| {
        let mut v = vec![tag, u8::try_from(s.len()).unwrap()];
        v.extend_from_slice(s.as_bytes());
        let parsed = tlv(&v).unwrap();
        time(&parsed)
    };
    assert_eq!(t(UTC_TIME, "700101000000Z"), Ok(0));
    assert_eq!(t(UTC_TIME, "491231235959Z"), Ok(2_524_607_999));
    assert_eq!(t(UTC_TIME, "500101000000Z"), Ok(-631_152_000));
    assert_eq!(t(GENERALIZED_TIME, "20500101000000Z"), Ok(2_524_608_000));
    for bad in [
        "700101000000+",
        "7001010000000",
        "701301000000Z",
        "70010100000aZ",
    ] {
        assert!(t(UTC_TIME, bad).is_err(), "{bad}");
    }
    assert!(t(GENERALIZED_TIME, "700101000000Z").is_err());
    // One field out of range alone refuses the time; the last valid value of each is accepted.
    for bad in [
        "700001000000Z", // month 0
        "701301000000Z", // month 13
        "700100000000Z", // day 0
        "700132000000Z", // day 32
        "700101240000Z", // hour 24
        "700101006000Z", // minute 60
        "700101000061Z", // second 61
    ] {
        assert!(t(UTC_TIME, bad).is_err(), "{bad}");
    }
    assert_eq!(t(UTC_TIME, "700101000000Z"), Ok(0));
    assert_eq!(t(UTC_TIME, "700131000000Z"), Ok(30 * 86_400));
    assert_eq!(t(UTC_TIME, "701201000000Z"), Ok(334 * 86_400));
    assert_eq!(t(UTC_TIME, "700101235959Z"), Ok(86_399));
    // A leap second is read as the next second.
    assert_eq!(t(UTC_TIME, "700101000060Z"), Ok(60));
    assert!(t(INTEGER, "700101000000Z").is_err());
}

#[test]
fn refuses_truncated_trailing_and_odd_encodings() {
    let der = cert("X", (2040, 1, 1));
    assert!(parse(&der[..der.len() - 1]).is_err());
    let mut trailing = der.clone();
    trailing.push(0);
    assert_eq!(parse(&trailing), Err(Malformed("trailing bytes")));
    assert_eq!(parse(&[]), Err(Malformed("truncated")));
    assert_eq!(parse(&[0x1f, 0x00]), Err(Malformed("multi-byte tag")));
    assert_eq!(parse(&[0x30, 0x80]), Err(Malformed("unsupported length")));
    assert_eq!(
        parse(&[0x30, 0x85, 1, 1, 1, 1, 1]),
        Err(Malformed("unsupported length"))
    );
    assert_eq!(
        parse(&[0x02, 0x00]),
        Err(Malformed("no certificate sequence"))
    );
    // Four length bytes are the most it reads: this one is supported and then runs short.
    assert_eq!(
        parse(&[0x30, 0x84, 0, 0, 0, 0]),
        Err(Malformed("truncated"))
    );
    assert_eq!(parse(&[0x30, 0x00]), Err(Malformed("truncated")));
    assert_eq!(
        parse(&[0x30, 0x02, 0x02, 0x00]),
        Err(Malformed("no tbsCertificate"))
    );
}

#[test]
fn common_name_handles_bmp_controls_and_absence() {
    // SET { SEQ { OID cn, BMPString "A" } }
    let bmp = [
        0x31, 0x0b, 0x30, 0x09, 0x06, 0x03, 0x55, 0x04, 0x03, 0x1e, 0x02, 0x00, 0x41,
    ];
    assert_eq!(common_name(&bmp).as_deref(), Some("A"));
    // An odd byte count: the trailing byte is dropped.
    let odd = [
        0x31, 0x0c, 0x30, 0x0a, 0x06, 0x03, 0x55, 0x04, 0x03, 0x1e, 0x03, 0x00, 0x41, 0x00,
    ];
    assert_eq!(common_name(&odd).as_deref(), Some("A"));
    let two = [
        0x31, 0x0d, 0x30, 0x0b, 0x06, 0x03, 0x55, 0x04, 0x03, 0x1e, 0x04, 0x00, 0x41, 0x00, 0x62,
    ];
    assert_eq!(common_name(&two).as_deref(), Some("Ab"));
    let ctl = [
        0x31, 0x0b, 0x30, 0x09, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0c, 0x02, b'a', 0x0a,
    ];
    assert_eq!(common_name(&ctl).as_deref(), Some("a?"));
    // Only an O attribute.
    let org = [
        0x31, 0x0a, 0x30, 0x08, 0x06, 0x03, 0x55, 0x04, 0x0a, 0x0c, 0x01, b'o',
    ];
    assert_eq!(common_name(&org), None);
    // A value that runs past its SET.
    let short = [0x31, 0x05, 0x30, 0x09, 0x06, 0x03, 0x55];
    assert_eq!(common_name(&short), None);
}

proptest! {
    #[test]
    fn any_bytes_parse_or_fail_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        let _ = parse(&bytes);
        let _ = common_name(&bytes);
    }

    #[test]
    fn a_corrupted_certificate_never_panics(index in 0_usize..400, value in any::<u8>()) {
        let mut der = cert("Corrupt", (2040, 1, 1));
        if let Some(b) = der.get_mut(index) {
            *b = value;
        }
        let _ = parse(&der);
    }
}
