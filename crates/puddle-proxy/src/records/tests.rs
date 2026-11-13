// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn plain_names_are_valid_and_everything_else_is_not() {
    for ok in [
        "a",
        "example.com",
        "_mongodb._tcp.c0.example.net",
        "xn--bcher-kva.example",
        "a-b.c-d",
    ] {
        assert!(valid_fqdn(ok), "{ok}");
    }
    for bad in [
        "",
        ".",
        "a..b",
        "a.b.",
        "Upper.example",
        "sp ace.example",
        "esc\u{1b}.example",
        "bücher.example",
        "star*.example",
        &"a".repeat(64),
        &format!(
            "{}.{}.{}.{}",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(63)
        ),
    ] {
        assert!(!valid_fqdn(bad), "{bad:?}");
    }
}

#[test]
fn record_targets_are_cleaned_and_the_root_is_dropped() {
    assert_eq!(
        target_name("Shard0.Example.NET."),
        Some("shard0.example.net".into())
    );
    assert_eq!(target_name("."), None);
    assert_eq!(target_name("bad name.example."), None);
}

#[test]
fn records_the_stub_cannot_carry_are_left_out_and_the_kept_ones_set_the_ttl() {
    use hickory_resolver::proto::rr::rdata::{MX, SRV, TXT};
    let name = |n: &str| hickory_resolver::proto::rr::Name::from_ascii(n).unwrap();
    let answers = [
        (
            RData::SRV(SRV::new(0, 1, 27017, name("shard0.example.net."))),
            60,
        ),
        // Not a name any client connects to: the root.
        (RData::SRV(SRV::new(0, 1, 0, name("."))), 5),
        (RData::MX(MX::new(10, name("mail.example.net."))), 30),
        (RData::TXT(TXT::new(vec!["a=b".into()])), 90),
        (RData::A(std::net::Ipv4Addr::LOCALHOST.into()), 1),
    ];
    let (records, ttl) = collect("example.net", answers.iter().map(|(d, t)| (d, *t)));
    assert_eq!(records.len(), 3, "{records:?}");
    assert_eq!(ttl, 30, "the root SRV and the A record set no ttl");
    let plain = [(RData::TXT(TXT::new(vec!["a=b".into()])), 20)];
    let (records, ttl) = collect("example.net", plain.iter().map(|(d, t)| (d, *t)));
    assert_eq!(
        (records.len(), ttl),
        (1, 20),
        "nothing left out, nothing said"
    );
    let many: Vec<_> = (0..MAX_RECORDS + 3)
        .map(|_| (RData::TXT(TXT::new(vec!["x".into()])), 100))
        .collect();
    let (records, _) = collect("example.net", many.iter().map(|(d, t)| (d, *t)));
    assert_eq!(records.len(), MAX_RECORDS);
}

#[test]
fn txt_is_kept_up_to_the_byte_cap_and_left_out_past_it() {
    use hickory_resolver::proto::rr::rdata::TXT;
    let txt = |len: usize| (RData::TXT(TXT::new(vec!["t".repeat(len)])), 60);
    let answers = [txt(8 * 1024), txt(1)];
    let (records, _) = collect("example.net", answers.iter().map(|(d, t)| (d, *t)));
    assert_eq!(
        records.len(),
        1,
        "a string of exactly the cap is kept, the next byte is not"
    );
}

#[test]
fn each_served_record_type_asks_for_its_own_dns_type() {
    assert_eq!(dns_type(RecordType::Srv), Some(DnsType::SRV));
    assert_eq!(dns_type(RecordType::Txt), Some(DnsType::TXT));
    assert_eq!(dns_type(RecordType::Mx), Some(DnsType::MX));
    assert_eq!(dns_type(RecordType::A), None);
}

#[tokio::test]
async fn an_address_type_is_no_data_for_the_record_resolver() {
    let err = SystemRecords
        .records("example.com", RecordType::A)
        .await
        .unwrap_err();
    assert_eq!(err, RecordError::NoData);
}
