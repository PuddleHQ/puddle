// SPDX-License-Identifier: AGPL-3.0-or-later
use super::super::{SecretValue, StandInOrigin, TerminationSet, TokenBody};
use super::*;
use puddle_types::QueryName;

const KEY: &str = "puddle-secret-OWM_KEY-00112233445566778899aabbccddeeff";
const BOT: &str = "puddle-secret-BOT_TOKEN-ffeeddccbbaa99887766554433221100";
const HDR: &str = "puddle-secret-API_KEY-0123456789abcdef0123456789abcdef";
const REAL_KEY: &str = "owm-real-1111";
const REAL_BOT: &str = "123456:ABC-real";
const REAL_HDR: &str = "sk-real-2222";

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn entry(name: &str, text: &str, real: &str, hosts: &[&str], placement: Placement) -> StandIn {
    StandIn::new(
        StandInOrigin::Secret,
        name,
        text,
        SecretValue::new(real),
        TerminationSet::parse(hosts.iter().copied()).unwrap(),
    )
    .unwrap()
    .placed(placement)
}

/// `OWM_KEY` in the query parameter `appid` toward `api.openweathermap.org`, `BOT_TOKEN` in
/// the path toward `api.telegram.org`, `API_KEY` in headers toward `api.example.com`.
fn registry() -> StandIns {
    let registry = StandIns::new();
    for entry in [
        entry(
            "OWM_KEY",
            KEY,
            REAL_KEY,
            &["api.openweathermap.org"],
            Placement::Query(QueryName::new("appid").unwrap()),
        ),
        entry(
            "BOT_TOKEN",
            BOT,
            REAL_BOT,
            &["api.telegram.org"],
            Placement::Path,
        ),
        entry(
            "API_KEY",
            HDR,
            REAL_HDR,
            &["api.example.com"],
            Placement::Header,
        ),
    ] {
        registry.insert(entry).unwrap();
    }
    registry
}

fn swap(registry: &StandIns, target: &str, to: &str) -> (Option<String>, Swapped) {
    let (new, report) = registry.swap_target(target, &host(to));
    (new.map(|new| new.as_str().to_owned()), report)
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter()
        .map(|name| format!("stand-in:secret:{name}"))
        .collect()
}

#[test]
fn a_query_stand_in_is_swapped_in_its_named_parameter_toward_its_hosts() {
    let registry = registry();
    let (new, report) = swap(
        &registry,
        &format!("/data/3.0/onecall?lat=1&appid={KEY}&units=metric"),
        "api.openweathermap.org",
    );
    assert_eq!(
        new.as_deref(),
        Some(format!("/data/3.0/onecall?lat=1&appid={REAL_KEY}&units=metric").as_str())
    );
    assert_eq!(report.swapped, ids(&["OWM_KEY"]));
    assert_eq!(report.unswapped, Vec::<String>::new());

    // The name and the value are read as a server reads them, escapes decoded.
    let escaped = KEY.replacen('-', "%2d", 2);
    let (new, report) = swap(
        &registry,
        &format!("/x?app%69d={escaped}"),
        "api.openweathermap.org",
    );
    assert_eq!(
        new.as_deref(),
        Some(format!("/x?app%69d={REAL_KEY}").as_str())
    );
    assert_eq!(report.swapped, ids(&["OWM_KEY"]));
}

#[test]
fn a_query_stand_in_anywhere_else_goes_out_unchanged_and_is_reported() {
    let registry = registry();
    for (target, to) in [
        // Toward a host it is not for.
        (format!("/x?appid={KEY}"), "evil.example.com"),
        (format!("/x?appid={KEY}"), "api.telegram.org"),
        // In another parameter, part of a value, a name, or the path.
        (format!("/x?key={KEY}"), "api.openweathermap.org"),
        (format!("/x?appid={KEY}x"), "api.openweathermap.org"),
        (format!("/x?appid=a{KEY}"), "api.openweathermap.org"),
        (format!("/x?{KEY}=appid"), "api.openweathermap.org"),
        (format!("/x?appid&{KEY}"), "api.openweathermap.org"),
        (format!("/{KEY}?appid=1"), "api.openweathermap.org"),
        // The named parameter given twice.
        (
            format!("/x?appid={KEY}&appid={KEY}"),
            "api.openweathermap.org",
        ),
        (
            format!("/x?appid={KEY}&appid=other"),
            "api.openweathermap.org",
        ),
        // A name that only looks alike.
        (format!("/x?APPID={KEY}"), "api.openweathermap.org"),
        (format!("/x?appid%20={KEY}"), "api.openweathermap.org"),
    ] {
        let (new, report) = swap(&registry, &target, to);
        assert_eq!(new, None, "{target} to {to}");
        assert_eq!(report.swapped, Vec::<String>::new(), "{target} to {to}");
        assert_eq!(report.unswapped, ids(&["OWM_KEY"]), "{target} to {to}");
    }
}

#[test]
fn a_path_stand_in_is_swapped_in_the_path_toward_its_hosts_and_nowhere_else() {
    let registry = registry();
    let (new, report) = swap(
        &registry,
        &format!("/bot{BOT}/sendMessage?chat_id=1"),
        "api.telegram.org",
    );
    assert_eq!(
        new.as_deref(),
        Some(format!("/bot{REAL_BOT}/sendMessage?chat_id=1").as_str())
    );
    assert_eq!(report.swapped, ids(&["BOT_TOKEN"]));
    assert_eq!(report.unswapped, Vec::<String>::new());

    // A whole segment, twice.
    let (new, _) = swap(&registry, &format!("/{BOT}/{BOT}"), "api.telegram.org");
    assert_eq!(
        new.as_deref(),
        Some(format!("/{REAL_BOT}/{REAL_BOT}").as_str())
    );

    for (target, to) in [
        (format!("/bot{BOT}/getMe"), "evil.example.com"),
        (format!("/getMe?token={BOT}"), "api.telegram.org"),
        (format!("/getMe?{BOT}"), "api.telegram.org"),
        // Escaped, it is not what the guest's tool would send: left, and reported.
        (
            format!("/bot{}/getMe", BOT.replacen('-', "%2D", 1)),
            "api.telegram.org",
        ),
    ] {
        let (new, report) = swap(&registry, &target, to);
        assert_eq!(new, None, "{target} to {to}");
        assert_eq!(report.unswapped, ids(&["BOT_TOKEN"]), "{target} to {to}");
    }
}

#[test]
fn a_header_stand_in_in_the_target_is_never_swapped_and_is_reported() {
    let registry = registry();
    for target in [
        format!("/a/{HDR}"),
        format!("/a?appid={HDR}"),
        format!("/a?key={HDR}"),
    ] {
        let (new, report) = swap(&registry, &target, "api.example.com");
        assert_eq!(new, None, "{target}");
        assert_eq!(report.unswapped, ids(&["API_KEY"]), "{target}");
    }
}

#[test]
fn a_swap_in_one_place_still_reports_a_copy_left_in_another() {
    let registry = registry();
    let (new, report) = swap(
        &registry,
        &format!("/bot{BOT}/x?appid={KEY}&copy={BOT}&h={HDR}"),
        "api.telegram.org",
    );
    assert_eq!(
        new.as_deref(),
        Some(format!("/bot{REAL_BOT}/x?appid={KEY}&copy={BOT}&h={HDR}").as_str())
    );
    assert_eq!(report.swapped, ids(&["BOT_TOKEN"]));
    assert_eq!(report.unswapped, ids(&["OWM_KEY", "BOT_TOKEN", "API_KEY"]));
}

#[test]
fn a_real_value_is_encoded_for_its_place_and_an_escape_in_it_is_kept() {
    let registry = StandIns::new();
    registry
        .insert(entry(
            "Q",
            KEY,
            "a b&c=d+e#f/g?h%2Bi%zz;",
            &["q.example.com"],
            Placement::Query(QueryName::new("sig").unwrap()),
        ))
        .unwrap();
    registry
        .insert(entry(
            "P",
            BOT,
            "a b/c?d#e%2Fx:y@z&;",
            &["q.example.com"],
            Placement::Path,
        ))
        .unwrap();
    let (new, _) = swap(&registry, &format!("/{BOT}?sig={KEY}"), "q.example.com");
    assert_eq!(
        new.as_deref(),
        Some("/a%20b/c%3Fd%23e%2Fx:y@z&%3B?sig=a%20b%26c%3Dd%2Be%23f/g?h%2Bi%25zz%3B")
    );
}

#[test]
fn a_query_parameter_without_a_value_and_an_empty_query_are_kept_as_sent() {
    let registry = registry();
    let (new, _) = swap(
        &registry,
        &format!("/x?flag&appid={KEY}&&end="),
        "api.openweathermap.org",
    );
    assert_eq!(
        new.as_deref(),
        Some(format!("/x?flag&appid={REAL_KEY}&&end=").as_str())
    );
    assert_eq!(
        swap(&registry, "/x?", "api.openweathermap.org"),
        (None, Swapped::default())
    );
    assert_eq!(
        swap(&registry, "/x", "api.telegram.org"),
        (None, Swapped::default())
    );
    assert_eq!(
        swap(&StandIns::new(), &format!("/bot{BOT}"), "api.telegram.org"),
        (None, Swapped::default())
    );
}

#[test]
fn a_target_stand_in_is_not_swapped_in_headers_or_a_refresh_field() {
    let registry = registry();
    let mut headers = ::http::HeaderMap::new();
    headers.insert("x-api-key", KEY.parse().unwrap());
    headers.insert("authorization", format!("Bearer {BOT}").parse().unwrap());
    let report = registry.swap(&mut headers, &host("api.openweathermap.org"), None);
    assert_eq!(headers["x-api-key"], KEY);
    assert_eq!(report.unswapped, ids(&["OWM_KEY", "BOT_TOKEN"]));
    assert_eq!(report.swapped, Vec::<String>::new());
    let report = registry.swap(&mut headers, &host("api.telegram.org"), None);
    assert_eq!(headers["authorization"], format!("Bearer {BOT}"));
    assert_eq!(report.swapped, Vec::<String>::new());

    let mut body = TokenBody::parse(
        Some("application/x-www-form-urlencoded"),
        format!("refresh_token={KEY}").as_bytes(),
    )
    .unwrap();
    let (report, changed) = registry.swap_fields(
        &mut body,
        &["refresh_token".to_owned()],
        &host("api.openweathermap.org"),
    );
    assert!(!changed);
    assert_eq!(report.unswapped, ids(&["OWM_KEY"]));
}

#[test]
fn the_audit_target_has_a_label_in_place_of_each_stand_in() {
    let registry = registry();
    assert_eq!(
        registry.redact_target(&format!("/bot{BOT}/x?appid={KEY}")),
        "/bot[secret:BOT_TOKEN]/x?appid=[secret:OWM_KEY]"
    );
    let escaped = format!("/a/{}", HDR.replacen('-', "%2D", 1));
    assert_eq!(registry.redact_target(&escaped), "/a/[secret:API_KEY]");
    assert!(matches!(
        registry.redact_target("/plain?x=%41"),
        Cow::Borrowed("/plain?x=%41")
    ));
    assert!(matches!(
        StandIns::new().redact_target(&format!("/{BOT}")),
        Cow::Borrowed(_)
    ));
}

#[test]
fn hostile_the_audit_target_keeps_every_escape_but_the_stand_ins_own() {
    let registry = registry();
    // Decoding the whole line would put a line break and a `?` in the record.
    let escaped = HDR.replacen('-', "%2D", 1);
    assert_eq!(
        registry.redact_target(&format!("/x%0d%0a%3Fy/{escaped}/a%2Fb?q={BOT}&r=%41")),
        "/x%0d%0a%3Fy/[secret:API_KEY]/a%2Fb?q=[secret:BOT_TOKEN]&r=%41"
    );
    // Overlapping copies: each label once, nothing of either stand-in left.
    assert_eq!(
        registry.redact_target(&format!("/{HDR}{escaped}")),
        "/[secret:API_KEY][secret:API_KEY]"
    );
}

#[test]
fn overlapping_stand_ins_in_the_audit_target_get_the_first_label() {
    // The end of one is the start of the other: neither contains the other, so both register.
    let registry = StandIns::new();
    for (name, text) in [
        ("FIRST", "prefix-aaaaaaaa-ZZZZ12345678"),
        ("SECOND", "ZZZZ12345678-bbbbbbbbbbbb"),
    ] {
        registry
            .insert(entry(
                name,
                text,
                "real",
                &["q.example.com"],
                Placement::Path,
            ))
            .unwrap();
    }
    assert_eq!(
        registry.redact_target("/prefix-aaaaaaaa-ZZZZ12345678-bbbbbbbbbbbb/x"),
        "/[secret:FIRST]-bbbbbbbbbbbb/x"
    );
}

#[test]
fn an_answer_header_that_repeats_a_target_secret_gets_the_stand_in_back() {
    let registry = StandIns::new();
    registry
        .insert(entry(
            "P",
            BOT,
            "a b/c",
            &["q.example.com"],
            Placement::Path,
        ))
        .unwrap();
    registry
        .insert(entry(
            "Q",
            KEY,
            "k&1",
            &["q.example.com"],
            Placement::Query(QueryName::new("sig").unwrap()),
        ))
        .unwrap();
    registry
        .insert(entry(
            "H",
            HDR,
            REAL_HDR,
            &["q.example.com"],
            Placement::Header,
        ))
        .unwrap();
    let mut headers = ::http::HeaderMap::new();
    let sent = "/a%20b/c/x?sig=k%261";
    headers.insert(
        "location",
        format!("https://q.example.com{sent}/").parse().unwrap(),
    );
    headers.insert("x-raw", "a b/c and k&1 and k%261".parse().unwrap());
    headers.insert("x-header-secret", REAL_HDR.parse().unwrap());
    headers.insert("x-plain", "nothing".parse().unwrap());
    let mut elsewhere = headers.clone();

    let hide = |registry: &StandIns, headers: &mut ::http::HeaderMap, to: &str| {
        if let Some(scrubber) = registry.answer_scrubber(&host(to)) {
            scrubber.scrub_headers(headers, &super::super::ScrubNotes::default());
        }
    };
    hide(&registry, &mut headers, "q.example.com");
    assert_eq!(
        headers["location"],
        format!("https://q.example.com/{BOT}/x?sig={KEY}/")
    );
    assert_eq!(headers["x-raw"], format!("{BOT} and {KEY} and {KEY}"));
    assert_eq!(headers["x-header-secret"], HDR, "a header secret too");
    assert_eq!(headers["x-plain"], "nothing");

    // Toward another host the secrets were never sent, so nothing is changed.
    let before = elsewhere.clone();
    hide(&registry, &mut elsewhere, "other.example.com");
    assert_eq!(elsewhere, before);
    hide(&StandIns::new(), &mut elsewhere, "q.example.com");
    assert_eq!(elsewhere, before);
}

#[test]
fn a_placement_is_the_header_unless_told_otherwise() {
    assert_eq!(Placement::default(), Placement::Header);
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn any_target_never_panics_and_without_a_stand_in_never_changes(
            target in "/[ -~]{0,80}",
        ) {
            let registry = registry();
            for to in ["api.openweathermap.org", "api.telegram.org"] {
                let (new, report) = swap(&registry, &target, to);
                prop_assert_eq!(new, None);
                prop_assert_eq!(report, Swapped::default());
            }
            prop_assert_eq!(registry.redact_target(&target), target.as_str());
        }

        #[test]
        fn a_swapped_target_never_holds_a_stand_in_for_its_host_in_its_place(
            before in "[a-z/]{0,10}",
            after in "[a-z=&]{0,10}",
        ) {
            let registry = registry();
            let target = format!("/{before}bot{BOT}/x?{after}&appid={KEY}");
            let (new, _) = swap(&registry, &target, "api.telegram.org");
            let new = new.unwrap();
            prop_assert!(!new.contains(BOT) && new.contains(REAL_BOT));
            prop_assert!(new.contains(KEY), "a query stand-in is not for this host");
        }
    }
}
