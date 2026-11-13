// SPDX-License-Identifier: AGPL-3.0-or-later
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
