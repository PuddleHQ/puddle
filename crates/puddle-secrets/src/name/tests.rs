// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn accepts_plain_names() {
    assert!(AccountName::new("TijsVK").is_ok());
    assert!(AccountName::new("me+x@example.com").is_ok());
    assert!(HostName::new("github.example.com:8443").is_ok());
    assert!(UrlPath::new("org/project/_git/repo").is_ok());
    assert!(StoredId::new("a1-b_2").is_ok());
}

#[test]
fn rejects_options_lines_and_odd_paths() {
    for bad in ["", "-x", "a b", "a\nb", "a\0b", "a;b", "a$b", "$(x)"] {
        assert!(AccountName::new(bad).is_err(), "{bad:?}");
    }
    assert!(AccountName::new("a".repeat(257)).is_err());
    assert!(HostName::new(".x").is_err());
    assert!(HostName::new("a/b").is_err());
    for bad in [
        "/a", "a/", "a//b", "a/../b", "a/./b", "a\nb", "a b", "a?x=1",
    ] {
        assert!(UrlPath::new(bad).is_err(), "{bad:?}");
    }
    assert!(StoredId::new("a".repeat(65)).is_err());
    assert!(StoredId::new("a:b").is_err());
}

#[test]
fn tenant_id_refuses_options_lines_and_batch_metacharacters() {
    assert!(TenantId::new("72f988bf-86f1-41af-91ab-2d7cd011db47").is_ok());
    assert!(TenantId::new("contoso.onmicrosoft.com").is_ok());
    for bad in [
        "", "-x", "--tenant", ".x", "a b", "a\nb", "a\0b", "a%b", "a^b", "a&b", "a\"b", "a|b",
        "a<b", "a>b", "a;b", "a$b", "a:b", "a_b", "a@b", "a/b", "a\\b", "a(b", "a!b",
    ] {
        assert!(TenantId::new(bad).is_err(), "{bad:?}");
    }
    assert!(TenantId::new("a".repeat(257)).is_err());
}

#[test]
fn serde_validates() {
    let ok: AccountName = serde_json::from_str("\"abc\"").unwrap();
    assert_eq!(ok.to_string(), "abc");
    assert_eq!(serde_json::to_string(&ok).unwrap(), "\"abc\"");
    assert!(serde_json::from_str::<AccountName>("\"-x\"").is_err());
}

#[test]
fn error_names_the_kind() {
    assert_eq!(
        AccountName::new("").unwrap_err().to_string(),
        "not a valid account name"
    );
}

#[test]
fn the_build_time_host_check_agrees_with_the_runtime_one() {
    let long = "a".repeat(257);
    let longest = "a".repeat(256);
    let samples = [
        &longest,
        "github.com",
        "dev.azure.com",
        "host:8443",
        "",
        "-x",
        ".x",
        "a b",
        "a\nb",
        "né.example",
        "a_b",
        &long,
    ];
    assert!(is_valid_host(&longest));
    for sample in samples {
        assert_eq!(
            is_valid_host(sample),
            HostName::new(sample).is_ok(),
            "{sample:?}"
        );
    }
}

#[test]
fn a_host_name_literal_is_the_same_host_name_as_a_parsed_one() {
    let lit = HostNameLit::new("dev.azure.com");
    assert_eq!(HostName::from(lit), HostName::new("dev.azure.com").unwrap());
}

#[test]
#[should_panic(expected = "not a valid host name")]
fn an_invalid_host_name_literal_panics_which_stops_a_const_build() {
    let _ = HostNameLit::new("a b");
}
