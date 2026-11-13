// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const GH: &str = "puddle-secret-GH_TOKEN-00112233445566778899aabbccddeeff";
const API: &str = "puddle-secret-API_KEY-ffeeddccbbaa99887766554433221100";
const REAL_GH: &str = "ghp_realRealReal1111";
const REAL_API: &str = "sk-real-2222";
const LOGIN: &str = "login-token-abcdefghijklmnop";
const NEW: &str = "puddle-secret-NEW-0000000000000000000000000000000a";

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn entry(name: &str, text: &str, real: &str, hosts: &[&str]) -> StandIn {
    StandIn::new(
        StandInOrigin::Secret,
        name,
        text,
        SecretValue::new(real),
        TerminationSet::parse(hosts.iter().copied()).unwrap(),
    )
    .unwrap()
}

fn registry() -> StandIns {
    let registry = StandIns::new();
    registry
        .insert(entry(
            "GH_TOKEN",
            GH,
            REAL_GH,
            &["github.com", "*.githubcopilot.com"],
        ))
        .unwrap();
    registry
        .insert(entry("API_KEY", API, REAL_API, &["api.example.com"]))
        .unwrap();
    registry
}

fn map(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

fn swap(registry: &StandIns, pairs: &[(&str, &str)], to: &str) -> (HeaderMap, Swapped) {
    let mut headers = map(pairs);
    let report = registry.swap(&mut headers, &host(to), None);
    (headers, report)
}

fn basic(text: &str) -> String {
    format!("Basic {}", STANDARD.encode(text))
}

fn decoded(value: &HeaderValue) -> String {
    let text = value.to_str().unwrap();
    let (scheme, rest) = text.split_once(' ').unwrap();
    assert!(scheme.eq_ignore_ascii_case("basic"), "{text}");
    String::from_utf8(LENIENT_BASE64.decode(rest).unwrap()).unwrap()
}

#[test]
fn an_entry_reports_the_placement_it_was_given() {
    let new = || entry("GH_TOKEN", GH, REAL_GH, &["api.example.com"]);
    assert_eq!(new().placement(), &Placement::Header);
    let query = QueryName::new("sig").unwrap();
    let placed = new().placed(Placement::Query(query.clone()));
    assert_eq!(placed.placement(), &Placement::Query(query));
    assert_eq!(new().placed(Placement::Path).placement(), &Placement::Path);
}

#[test]
fn a_stand_in_is_swapped_in_any_header_value_toward_its_hosts() {
    let registry = registry();
    let (headers, report) = swap(
        &registry,
        &[
            ("authorization", &format!("Bearer {GH}")),
            ("x-api-key", GH),
            ("cookie", &format!("a=1; token={GH}; b=2")),
            ("user-agent", "git/2.47"),
        ],
        "github.com",
    );
    assert_eq!(headers["authorization"], format!("Bearer {REAL_GH}"));
    assert_eq!(headers["x-api-key"], REAL_GH);
    assert_eq!(headers["cookie"], format!("a=1; token={REAL_GH}; b=2"));
    assert_eq!(headers["user-agent"], "git/2.47");
    assert_eq!(report.swapped, ["stand-in:secret:GH_TOKEN"]);
    assert_eq!(report.unswapped, Vec::<String>::new());
    assert!(headers["authorization"].is_sensitive());
    assert!(!headers["user-agent"].is_sensitive());
}

fn login(name: &str, text: &str, real: &str) -> StandIn {
    StandIn::new(
        StandInOrigin::CapturedLogin,
        name,
        text,
        SecretValue::new(real),
        TerminationSet::parse(["login.example.com"]).unwrap(),
    )
    .unwrap()
}

#[test]
fn replacing_an_origin_changes_its_entries_in_one_step_and_leaves_the_others() {
    let registry = registry();
    registry
        .insert(login("claude", LOGIN, "real-login"))
        .unwrap();
    assert_eq!(
        registry.ids(),
        [
            "stand-in:secret:GH_TOKEN",
            "stand-in:secret:API_KEY",
            "stand-in:login:claude"
        ]
    );
    // GH_TOKEN stays with a new real value and hosts, API_KEY goes, NEW comes.
    registry
        .replace_origin(
            StandInOrigin::Secret,
            vec![
                entry("GH_TOKEN", GH, "ghp_rotated", &["gitlab.com"]),
                entry("NEW", NEW, "real-new", &["new.example.com"]),
            ],
        )
        .unwrap();
    assert_eq!(
        registry.ids(),
        [
            "stand-in:login:claude",
            "stand-in:secret:GH_TOKEN",
            "stand-in:secret:NEW"
        ]
    );
    let (headers, _) = swap(&registry, &[("a", GH)], "gitlab.com");
    assert_eq!(headers["a"], "ghp_rotated");
    let (headers, report) = swap(&registry, &[("a", GH)], "github.com");
    assert_eq!(headers["a"], GH, "its old host no longer swaps it");
    assert_eq!(report.unswapped, ["stand-in:secret:GH_TOKEN"]);
    let (headers, _) = swap(&registry, &[("a", API)], "api.example.com");
    assert_eq!(headers["a"], API, "a removed stand-in is just text");
    let (headers, _) = swap(&registry, &[("a", LOGIN)], "login.example.com");
    assert_eq!(
        headers["a"], "real-login",
        "the login entry was not touched"
    );
    let hosts = registry.hosts();
    assert!(hosts.contains(&host("new.example.com")));
    assert!(hosts.contains(&host("login.example.com")));
    assert!(!hosts.contains(&host("api.example.com")));
    // An empty list removes every entry of the origin.
    registry
        .replace_origin(StandInOrigin::Secret, Vec::new())
        .unwrap();
    assert_eq!(registry.ids(), ["stand-in:login:claude"]);
}

#[test]
fn a_replacement_that_overlaps_or_is_of_another_origin_changes_nothing() {
    let registry = registry();
    let before = registry.ids();
    let twin = format!("{GH}-more-text");
    for bad in [
        vec![
            entry("A", GH, "x1", &["a.example.com"]),
            entry("B", &twin, "x2", &["b.example.com"]),
        ],
        vec![login("claude", "login-token-abcdefghijklmnop", "r")],
    ] {
        assert!(registry.replace_origin(StandInOrigin::Secret, bad).is_err());
        assert_eq!(registry.ids(), before);
    }
    // A secret's stand-in that equals a login's is refused too.
    registry
        .insert(login("claude", "login-token-abcdefghijklmnop", "r"))
        .unwrap();
    let clash = entry("C", "login-token-abcdefghijklmnop", "x", &["c.example.com"]);
    assert_eq!(
        registry.replace_origin(StandInOrigin::Secret, vec![clash]),
        Err(StandInError::Overlaps)
    );
    assert_eq!(registry.len(), 3);
}

#[test]
fn a_pattern_host_and_a_name_below_it_are_covered() {
    let registry = registry();
    let (headers, _) = swap(&registry, &[("authorization", GH)], "api.githubcopilot.com");
    assert_eq!(headers["authorization"], REAL_GH);
    let (headers, report) = swap(&registry, &[("authorization", GH)], "githubcopilot.com");
    assert_eq!(
        headers["authorization"], GH,
        "the suffix itself is not below it"
    );
    assert_eq!(report.unswapped, ["stand-in:secret:GH_TOKEN"]);
}

#[test]
fn several_stand_ins_and_copies_in_one_value_are_all_swapped_once() {
    let registry = StandIns::new();
    registry
        .insert(entry("GH_TOKEN", GH, REAL_GH, &["github.com"]))
        .unwrap();
    registry
        .insert(entry("API_KEY", API, REAL_API, &["github.com"]))
        .unwrap();
    let value = format!("{GH},{API},{GH}");
    let (headers, report) = swap(&registry, &[("x-multi", &value)], "github.com");
    assert_eq!(
        headers["x-multi"],
        format!("{REAL_GH},{REAL_API},{REAL_GH}")
    );
    assert_eq!(report.swapped.len(), 2);
}

#[test]
fn stand_ins_that_overlap_in_a_value_swap_the_first_and_keep_the_rest_of_the_other() {
    // The end of one is the start of the other: neither contains the other, so both register.
    let first = "prefix-aaaaaaaa-ZZZZ12345678";
    let second = "ZZZZ12345678-bbbbbbbbbbbb";
    let registry = StandIns::new();
    registry
        .insert(entry("FIRST", first, "real-first", &["github.com"]))
        .unwrap();
    registry
        .insert(entry("SECOND", second, "real-second", &["github.com"]))
        .unwrap();
    let sent = "prefix-aaaaaaaa-ZZZZ12345678-bbbbbbbbbbbb";
    let (headers, report) = swap(&registry, &[("x-value", sent)], "github.com");
    assert_eq!(headers["x-value"], "real-first-bbbbbbbbbbbb");
    assert_eq!(report.swapped, ["stand-in:secret:FIRST"]);
}

#[test]
fn a_real_value_is_never_searched_for_stand_ins() {
    // The real value of one entry contains the stand-in of the other: one pass, no cascade.
    let registry = StandIns::new();
    registry
        .insert(entry("API_KEY", API, &format!("see-{GH}"), &["github.com"]))
        .unwrap();
    registry
        .insert(entry("GH_TOKEN", GH, REAL_GH, &["github.com"]))
        .unwrap();
    let (headers, _) = swap(&registry, &[("x-k", API)], "github.com");
    assert_eq!(headers["x-k"], format!("see-{GH}"));
}

#[test]
fn only_whole_stand_ins_match() {
    let registry = registry();
    let truncated = &GH[..GH.len() - 1];
    let other_random = "puddle-secret-GH_TOKEN-ffffffffffffffffffffffffffffffff";
    let near = format!("{}{}", &GH[..GH.len() - 2], "zz");
    for value in [
        truncated.to_owned(),
        other_random.to_owned(),
        near,
        GH.to_ascii_uppercase(),
        "puddle-secret".to_owned(),
    ] {
        let (headers, report) = swap(&registry, &[("authorization", &value)], "github.com");
        assert_eq!(headers["authorization"], value.as_str(), "{value}");
        // Only the whole value of the right shape is flagged as stale: the near misses
        // that are not a stand-in's shape are plain text.
        assert_eq!(report.swapped, Vec::<String>::new(), "{value}");
        assert!(
            report
                .unswapped
                .iter()
                .all(|id| id.starts_with("stale-stand-in:secret:")),
            "{value}"
        );
    }
    // Text around a whole one is kept as it is.
    let (headers, _) = swap(
        &registry,
        &[("authorization", &format!("x{GH}y"))],
        "github.com",
    );
    assert_eq!(headers["authorization"], format!("x{REAL_GH}y"));
}

#[test]
fn a_stand_in_toward_another_host_goes_out_unchanged_and_is_reported() {
    let registry = registry();
    let (headers, report) = swap(
        &registry,
        &[("authorization", &format!("Bearer {API}")), ("x-gh", GH)],
        "github.com",
    );
    assert_eq!(headers["authorization"], format!("Bearer {API}"));
    assert_eq!(headers["x-gh"], REAL_GH, "the other one is still swapped");
    assert_eq!(report.unswapped, ["stand-in:secret:API_KEY"]);
    assert_eq!(report.swapped, ["stand-in:secret:GH_TOKEN"]);
    assert!(!headers["authorization"].is_sensitive());
}

#[test]
fn a_stand_in_this_workspace_does_not_know_is_left_alone_and_flagged_as_stale() {
    // Another workspace's stand-in for the same secret name.
    let theirs = "puddle-secret-GH_TOKEN-0123456789abcdef0123456789abcdef";
    let registry = registry();
    let (headers, report) = swap(&registry, &[("authorization", theirs)], "github.com");
    assert_eq!(headers["authorization"], theirs);
    assert_eq!(report.swapped, Vec::<String>::new());
    assert_eq!(report.unswapped, ["stale-stand-in:secret:GH_TOKEN"]);
}

#[test]
fn a_basic_value_is_decoded_swapped_and_encoded_again() {
    let registry = registry();
    for credentials in [
        basic(&format!("x-access-token:{GH}")),
        basic(&format!("{GH}:")),
        basic(&format!("oauth2:{GH}")).replace('=', ""),
    ] {
        let (headers, report) = swap(&registry, &[("authorization", &credentials)], "github.com");
        let value = decoded(&headers["authorization"]);
        assert!(value.contains(REAL_GH) && !value.contains(GH), "{value}");
        assert_eq!(report.swapped, ["stand-in:secret:GH_TOKEN"]);
        assert!(headers["authorization"].is_sensitive());
    }
    let (headers, _) = swap(
        &registry,
        &[(
            "authorization",
            &format!("basic   {}", STANDARD.encode(format!("u:{GH}"))),
        )],
        "github.com",
    );
    assert_eq!(decoded(&headers["authorization"]), format!("u:{REAL_GH}"));
    assert!(
        headers["authorization"]
            .to_str()
            .unwrap()
            .starts_with("basic ")
    );
}

#[test]
fn a_basic_value_for_another_host_is_left_as_it_was_sent() {
    let registry = registry();
    let sent = basic(&format!("u:{API}"));
    let (headers, report) = swap(&registry, &[("authorization", &sent)], "github.com");
    assert_eq!(headers["authorization"], sent.as_str());
    assert_eq!(report.unswapped, ["stand-in:secret:API_KEY"]);
    assert_eq!(report.swapped, Vec::<String>::new());
    // One bound and one unbound inside the same value: the bound one is swapped, the other kept.
    let sent = basic(&format!("{GH}:{API}"));
    let (headers, report) = swap(&registry, &[("authorization", &sent)], "github.com");
    assert_eq!(
        decoded(&headers["authorization"]),
        format!("{REAL_GH}:{API}")
    );
    assert_eq!(report.unswapped, ["stand-in:secret:API_KEY"]);
}

#[test]
fn a_basic_value_that_is_not_credentials_is_left_alone() {
    let registry = registry();
    for sent in [
        "Basic".to_owned(),
        "Basic ".to_owned(),
        "Basicx dXNlcjpwdw==".to_owned(),
        // The scheme must be followed by a space: credentials glued to it are not a Basic value.
        format!("Basic{}", STANDARD.encode(format!("u:{GH}"))),
        "Basic not base64 at all!".to_owned(),
        "Basic dXNlcjpwdw==".to_owned(),
        format!("Basic {}", STANDARD.encode([0xff_u8, 0xfe, 0x00])),
        "Bearer abc".to_owned(),
    ] {
        let (headers, report) = swap(&registry, &[("authorization", &sent)], "github.com");
        assert_eq!(headers["authorization"], sent.as_str(), "{sent}");
        assert_eq!(report, Swapped::default(), "{sent}");
    }
}

#[test]
fn a_basic_value_with_bytes_that_are_not_text_still_swaps() {
    let registry = registry();
    let mut raw = vec![0xff_u8, b':'];
    raw.extend_from_slice(GH.as_bytes());
    let sent = format!("Basic {}", STANDARD.encode(&raw));
    let (headers, _) = swap(&registry, &[("authorization", &sent)], "github.com");
    let value = headers["authorization"].to_str().unwrap();
    let bytes = LENIENT_BASE64
        .decode(value.strip_prefix("Basic ").unwrap())
        .unwrap();
    let mut expected = vec![0xff_u8, b':'];
    expected.extend_from_slice(REAL_GH.as_bytes());
    assert_eq!(bytes, expected);
}

#[test]
fn headers_an_injector_set_are_never_scanned() {
    use super::super::inject::{InjectedHeader, Injection};
    let registry = registry();
    let injection = Injection::new(
        "b1",
        vec![
            InjectedHeader::new("authorization", SecretValue::new(format!("Bearer {GH}"))).unwrap(),
        ],
    );
    let mut headers = map(&[("authorization", &format!("Bearer {GH}")), ("x-other", GH)]);
    let report = registry.swap(&mut headers, &host("github.com"), Some(&injection));
    assert_eq!(headers["authorization"], format!("Bearer {GH}"));
    assert_eq!(headers["x-other"], REAL_GH);
    assert_eq!(report.swapped, ["stand-in:secret:GH_TOKEN"]);
}

#[test]
fn nothing_registered_nothing_changes() {
    let registry = StandIns::new();
    assert!(registry.is_empty());
    assert!(!self::registry().is_empty());
    let (headers, report) = swap(&registry, &[("authorization", GH)], "github.com");
    assert_eq!(headers["authorization"], GH);
    assert_eq!(report.swapped, Vec::<String>::new());
    assert_eq!(
        report.unswapped,
        ["stale-stand-in:secret:GH_TOKEN"],
        "a removed secret's stand-in is flagged even when nothing is registered"
    );
    let (_, plain) = swap(&registry, &[("authorization", "Bearer abc")], "github.com");
    assert_eq!(plain, Swapped::default());
}

#[test]
fn a_value_full_of_copies_is_swapped_without_blowing_up() {
    let registry = StandIns::new();
    for i in 0..64 {
        let text = format!("puddle-secret-N{i}-00112233445566778899aabbccddeeff");
        registry
            .insert(entry(&format!("N{i}"), &text, "r", &["github.com"]))
            .unwrap();
    }
    registry
        .insert(entry("GH_TOKEN", GH, REAL_GH, &["github.com"]))
        .unwrap();
    let many = vec![GH; 1000].join(",");
    let (headers, report) = swap(&registry, &[("x-many", &many)], "github.com");
    assert_eq!(headers["x-many"], vec![REAL_GH; 1000].join(",").as_str());
    assert_eq!(report.swapped.len(), 1);
}

#[test]
fn registering_is_checked() {
    let hosts = || TerminationSet::parse(["github.com"]).unwrap();
    let new = |name: &str, text: &str, real: &str, hosts: TerminationSet| {
        StandIn::new(
            StandInOrigin::Secret,
            name,
            text,
            SecretValue::new(real),
            hosts,
        )
    };
    assert!(matches!(
        new("", GH, "r", hosts()),
        Err(StandInError::Name(_))
    ));
    assert!(matches!(
        new("a b", GH, "r", hosts()),
        Err(StandInError::Name(_))
    ));
    assert!(matches!(
        new(&"n".repeat(65), GH, "r", hosts()),
        Err(StandInError::Name(_))
    ));
    for text in [
        "short",
        "has a space in it, long enough",
        "tab\tin-it-and-long-enough",
        &"x".repeat(MAX_LEN + 1),
    ] {
        assert_eq!(
            new("N", text, "r", hosts()).unwrap_err(),
            StandInError::Shape,
            "{text:?}"
        );
    }
    // A Copilot session token is a few hundred characters: the longest stand-in is accepted.
    assert!(new("N", &"x".repeat(MAX_LEN), "r", hosts()).is_ok());
    assert_eq!(
        new("N", GH, "r", TerminationSet::new()).unwrap_err(),
        StandInError::NoHosts
    );
    for shared in [
        "*.github.io",
        "*.co.uk",
        "*.azurewebsites.net",
        "*.s3.amazonaws.com",
    ] {
        let wide = TerminationSet::parse(["github.com", shared]).unwrap();
        assert!(
            matches!(new("N", GH, "r", wide), Err(StandInError::SharedDomain(_))),
            "{shared}"
        );
    }
    // An exact name below a shared domain is the user's own choice; a pattern below a
    // registrable domain is fine.
    for fine in ["a.github.io", "*.githubcopilot.com", "*.example.co.uk"] {
        let set = TerminationSet::parse([fine]).unwrap();
        assert!(new("N", GH, "r", set).is_ok(), "{fine}");
    }
    for real in ["", "a\r\nb: c", "a\0b"] {
        assert_eq!(
            new("N", GH, real, hosts()).unwrap_err(),
            StandInError::Value,
            "{real:?}"
        );
    }
    let registry = registry();
    assert_eq!(
        registry.insert(entry("DUP", GH, "r", &["a.example.com"])),
        Err(StandInError::Overlaps)
    );
    let longer = format!("{GH}-more");
    assert_eq!(
        registry.insert(entry("LONG", &longer, "r", &["a.example.com"])),
        Err(StandInError::Overlaps)
    );
    assert_eq!(
        registry.insert(entry("SHORT", &GH[..20], "r", &["a.example.com"])),
        Err(StandInError::Overlaps)
    );
    assert_eq!(registry.len(), 2);
}

#[test]
fn the_registry_changes_while_in_use() {
    let registry = registry();
    assert!(registry.remove(API));
    assert!(!registry.remove(API));
    assert_eq!(registry.len(), 1);
    let (headers, report) = swap(&registry, &[("x", API)], "api.example.com");
    assert_eq!(headers["x"], API);
    assert_eq!(report.swapped, Vec::<String>::new());
    assert_eq!(
        report.unswapped,
        ["stale-stand-in:secret:API_KEY"],
        "a removed stand-in is flagged as stale"
    );

    assert_eq!(
        registry.set_real(GH, SecretValue::new("ghp_refreshed")),
        Ok(true)
    );
    assert_eq!(
        registry.set_real("nope-nope-nope-nope", SecretValue::new("x")),
        Ok(false)
    );
    assert_eq!(
        registry.set_real(GH, SecretValue::new("a\nb")),
        Err(StandInError::Value)
    );
    let (headers, _) = swap(&registry, &[("x", GH)], "github.com");
    assert_eq!(headers["x"], "ghp_refreshed");
}

#[test]
fn the_hosts_to_terminate_are_the_hosts_of_every_entry() {
    let registry = registry();
    let hosts = registry.hosts();
    for yes in ["github.com", "x.githubcopilot.com", "api.example.com"] {
        assert!(hosts.contains(&host(yes)), "{yes}");
    }
    assert!(!hosts.contains(&host("example.com")));
    assert!(StandIns::new().hosts().is_empty());
}

#[test]
fn debug_shows_neither_the_stand_in_nor_the_real_value() {
    let text = format!(
        "{:?} {:?}",
        registry(),
        entry("GH_TOKEN", GH, REAL_GH, &["github.com"])
    );
    assert!(
        !text.contains(REAL_GH) && !text.contains(&GH[GH.len() - 12..]),
        "{text}"
    );
    assert!(text.contains("stand-in:secret:GH_TOKEN"));
    let id = entry("login", API, "r", &["a.example.com"]);
    assert_eq!(id.id(), "stand-in:secret:login");
    assert_eq!(id.origin(), StandInOrigin::Secret);
    assert!(id.hosts().contains(&host("a.example.com")));
    let captured = StandIn::new(
        StandInOrigin::CapturedLogin,
        "github",
        "ghp_0123456789abcdefghij",
        SecretValue::new("x"),
        TerminationSet::parse(["github.com"]).unwrap(),
    )
    .unwrap();
    assert_eq!(captured.id(), "stand-in:login:github");
}

mod properties {
    use proptest::prelude::*;

    use super::*;

    /// Pieces of a header value. The text has no hex digit, so no text can finish a cut-off
    /// stand-in into a whole one.
    #[derive(Debug, Clone)]
    enum Piece {
        Text(String),
        /// The stand-in that is for `github.com`.
        Bound,
        /// The stand-in that is for another host.
        Unbound,
        /// The bound stand-in with its last character gone.
        CutOff,
    }

    fn piece() -> impl Strategy<Value = Piece> {
        prop_oneof![
            "[G-Z ,;=]{0,20}".prop_map(Piece::Text),
            Just(Piece::Bound),
            Just(Piece::Unbound),
            Just(Piece::CutOff),
        ]
    }

    /// The value as the guest sends it, and as the real server must get it.
    fn build(pieces: &[Piece]) -> (String, String) {
        let (mut sent, mut expected) = (String::new(), String::new());
        for piece in pieces {
            let (from, to) = match piece {
                Piece::Text(text) => (text.as_str(), text.as_str()),
                Piece::Bound => (GH, REAL_GH),
                Piece::Unbound => (API, API),
                Piece::CutOff => (&GH[..GH.len() - 1], &GH[..GH.len() - 1]),
            };
            sent.push_str(from);
            expected.push_str(to);
        }
        (sent, expected)
    }

    proptest! {
        #[test]
        fn a_value_built_from_pieces_comes_out_with_only_the_bound_ones_swapped(
            pieces in proptest::collection::vec(piece(), 0..12),
        ) {
            let registry = registry();
            let (sent, expected) = build(&pieces);
            let (headers, report) = swap(&registry, &[("x-value", &sent)], "github.com");
            prop_assert_eq!(headers["x-value"].to_str().unwrap(), expected.as_str());
            let has = |wanted: fn(&Piece) -> bool| pieces.iter().any(wanted);
            prop_assert_eq!(
                report.swapped.is_empty(),
                !has(|p| matches!(p, Piece::Bound))
            );
            prop_assert_eq!(
                report.unswapped.is_empty(),
                !has(|p| matches!(p, Piece::Unbound))
            );
        }

        #[test]
        fn the_same_inside_a_basic_value(
            pieces in proptest::collection::vec(piece(), 0..12),
        ) {
            let registry = registry();
            let (sent, expected) = build(&pieces);
            let basic = format!("Basic {}", STANDARD.encode(format!("user:{sent}")));
            let (headers, _) = swap(&registry, &[("authorization", &basic)], "github.com");
            let text = headers["authorization"].to_str().unwrap();
            let credentials = text.strip_prefix("Basic ").unwrap();
            prop_assert_eq!(
                String::from_utf8(LENIENT_BASE64.decode(credentials).unwrap()).unwrap(),
                format!("user:{expected}")
            );
        }

        #[test]
        fn any_bytes_in_a_header_never_panic_and_without_a_stand_in_never_change(
            bytes in proptest::collection::vec(any::<u8>(), 0..200),
        ) {
            let registry = registry();
            let mut headers = HeaderMap::new();
            if let Ok(value) = HeaderValue::from_bytes(&bytes) {
                headers.insert("x-value", value.clone());
                registry.swap(&mut headers, &host("github.com"), None);
                prop_assert_eq!(&headers["x-value"], &value);
            }
        }
    }
}

#[test]
fn generated_stand_ins_have_the_documented_shape_and_differ() {
    let a = secret_stand_in("OPENAI_API_KEY").unwrap();
    let b = secret_stand_in("OPENAI_API_KEY").unwrap();
    assert_ne!(a, b);
    let random = a.strip_prefix("puddle-secret-OPENAI_API_KEY-").unwrap();
    assert_eq!(random.len(), 32);
    assert!(random.bytes().all(|c| c.is_ascii_hexdigit()));
    // The shortest name still makes a stand-in `StandIn::new` accepts.
    let short = secret_stand_in("A").unwrap();
    entry("A", &short, "r", &["github.com"]);
    assert_eq!(
        stand_in_with("A", &[0xab; RANDOM_BYTES]).unwrap(),
        format!("puddle-secret-A-{}", "ab".repeat(16))
    );
    for bad in ["", "with space", "a/b", &"n".repeat(65)] {
        assert!(
            matches!(secret_stand_in(bad), Err(StandInError::Name(_))),
            "{bad:?}"
        );
    }
}

fn token_body(content_type: &str, text: &str) -> TokenBody {
    TokenBody::parse(Some(content_type), text.as_bytes()).unwrap()
}

#[test]
fn a_stand_in_in_a_named_body_field_is_swapped_toward_its_hosts_only() {
    let registry = registry();
    let fields = vec!["refresh_token".to_owned()];
    let json = format!(r#"{{"refresh_token":"{GH}","other":"{GH}"}}"#);

    let mut body = token_body("application/json", &json);
    let (report, changed) = registry.swap_fields(&mut body, &fields, &host("github.com"));
    assert!(changed);
    assert_eq!(report.swapped, ["stand-in:secret:GH_TOKEN"]);
    let sent: serde_json::Value = serde_json::from_slice(&body.render()).unwrap();
    assert_eq!(sent["refresh_token"], REAL_GH);
    assert_eq!(sent["other"], GH, "only the named field");

    // Toward a host it is not for: left, and reported.
    let mut body = token_body("application/json", &json);
    let (report, changed) = registry.swap_fields(&mut body, &fields, &host("api.example.com"));
    assert!(!changed);
    assert_eq!(report.unswapped, ["stand-in:secret:GH_TOKEN"]);
    assert_eq!(report.swapped, Vec::<String>::new());
    // Nothing changed, so the caller sends the body it read, not this rendering.
    let unchanged: serde_json::Value = serde_json::from_slice(&body.render()).unwrap();
    assert_eq!(unchanged["refresh_token"], GH);
}

#[test]
fn a_body_field_that_is_not_a_known_stand_in_is_left_alone_in_either_format() {
    let registry = registry();
    let fields = vec!["refresh_token".to_owned(), "other_field".to_owned()];
    for (content_type, text) in [
        (
            "application/json",
            r#"{"refresh_token":"ghp_not_a_stand_in"}"#.to_owned(),
        ),
        (
            "application/json",
            r#"{"refresh_token":null,"x":1}"#.to_owned(),
        ),
        (
            "application/x-www-form-urlencoded",
            "refresh_token=ghp_not_a_stand_in".to_owned(),
        ),
        (
            "application/x-www-form-urlencoded",
            format!("refresh_token={GH}&refresh_token={GH}"),
        ),
        (
            "application/x-www-form-urlencoded",
            format!("refresh_token_x={GH}"),
        ),
    ] {
        let mut body = token_body(content_type, &text);
        let (report, changed) = registry.swap_fields(&mut body, &fields, &host("github.com"));
        assert!(!changed, "{text}");
        assert_eq!(report, Swapped::default(), "{text}");
    }
    // A form value is decoded before it is compared, and written back encoded.
    let encoded = GH.replace('-', "%2D");
    let mut body = token_body(
        "application/x-www-form-urlencoded",
        &format!("a=1&refresh_token={encoded}"),
    );
    let (_, changed) = registry.swap_fields(&mut body, &fields, &host("github.com"));
    assert!(changed);
    assert_eq!(
        *body.render(),
        format!("a=1&refresh_token={REAL_GH}").as_bytes()
    );
}

#[test]
fn a_target_is_scanned_as_sent_and_percent_decoded_and_known_stand_ins_are_named() {
    let registry = registry();
    let escaped = GH.replacen('-', "%2D", 1);
    let report = registry.scan_target(&format!("/a/{API}?t={escaped}&u=puddle-secret-ZZZ-00"));
    assert_eq!(
        report.unswapped,
        ["stand-in:secret:GH_TOKEN", "stand-in:secret:API_KEY"]
    );
    assert_eq!(report.swapped, Vec::<String>::new());
    assert_eq!(
        registry.scan_target("/plain?x=1%2").unswapped,
        Vec::<String>::new()
    );
    assert_eq!(
        StandIns::new().scan_target(&format!("/{GH}")).unswapped,
        ["stale-stand-in:secret:GH_TOKEN"]
    );
    assert_eq!(
        StandIns::new().scan_target("/plain").unswapped,
        Vec::<String>::new()
    );
}

#[test]
fn percent_decoding_leaves_a_broken_escape_and_ignores_input_without_one() {
    assert_eq!(percent_decoded(b"/plain"), None);
    assert_eq!(percent_decoded(b"a%41%2g%"), Some(b"aA%2g%".to_vec()));
    assert_eq!(percent_decoded(b"%ff"), Some(vec![0xff]));
}
