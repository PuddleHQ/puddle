// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn names_are_shell_identifiers_that_puddle_does_not_own() {
    for ok in [
        "A",
        "_x",
        "NPM_TOKEN",
        "a1",
        "JAVA_TOOL_OPTIONS",
        "MAVEN_ARGS",
    ] {
        assert!(EnvName::new(ok).is_ok(), "{ok}");
    }
    for bad in ["", "1A", "A-B", "A B", "A=B", "ä", &"A".repeat(65)] {
        assert!(EnvName::new(bad).is_err(), "{bad:?}");
    }
    assert!(EnvName::new(&"A".repeat(64)).is_ok());
}

#[test]
fn a_name_puddle_owns_is_refused_with_the_reason() {
    for name in PUDDLE_OWNED_NAMES {
        let err = EnvName::new(name).unwrap_err().to_string();
        assert!(err.contains("Puddle sets"), "{name}: {err}");
    }
    assert!(
        EnvName::new("PUDDLE_ROOT")
            .unwrap_err()
            .to_string()
            .contains("PUDDLE_")
    );
    assert!(
        EnvName::new("PATH")
            .unwrap_err()
            .to_string()
            .contains("image")
    );
}

#[test]
fn the_owned_names_are_sorted_and_unique() {
    let mut sorted = PUDDLE_OWNED_NAMES;
    sorted.sort_unstable();
    assert_eq!(sorted, PUDDLE_OWNED_NAMES);
    let all: BTreeSet<_> = PUDDLE_OWNED_NAMES.into_iter().collect();
    assert_eq!(all.len(), PUDDLE_OWNED_NAMES.len());
}

#[test]
fn hosts_are_names_or_wildcards_in_lower_case() {
    for (given, stored) in [
        ("API.Example.com", "api.example.com"),
        ("*.example.com", "*.example.com"),
        (" github.com ", "github.com"),
        ("*.visualstudio.com", "*.visualstudio.com"),
    ] {
        assert_eq!(SecretHost::new(given).unwrap().as_str(), stored, "{given}");
    }
}

#[test]
fn a_host_that_is_not_a_plain_name_is_refused_with_a_reason() {
    for bad in [
        "",
        "10.0.0.1",
        "[::1]",
        "api.example.com:8443",
        "https://example.com",
        "example.com/path",
        "user@example.com",
        ".example.com",
        "*.com",
        "*.co.uk",
        "*.github.io",
        "a b.example.com",
        "*",
    ] {
        assert!(SecretHost::new(bad).is_err(), "{bad:?}");
    }
    let shared = SecretHost::new("*.github.io").unwrap_err().to_string();
    assert!(shared.contains("anyone can register"), "{shared}");
    let address = SecretHost::new("10.0.0.1").unwrap_err().to_string();
    assert!(address.contains("host name"), "{address}");
}

#[test]
fn a_place_is_a_header_a_path_or_a_query_parameter_with_a_checked_name() {
    assert_eq!(SecretPlace::default(), SecretPlace::Header);
    for good in ["appid", "access_token", "key", "a[b]", "x.y-z~"] {
        let place = SecretPlace::Query(QueryName::new(good).unwrap());
        assert_eq!(
            SecretPlace::from_column(place.column().as_deref()).unwrap(),
            place
        );
    }
    for (place, column) in [
        (SecretPlace::Header, None),
        (SecretPlace::Path, Some("path")),
        (
            SecretPlace::Query(QueryName::new("appid").unwrap()),
            Some("query:appid"),
        ),
    ] {
        assert_eq!(place.column().as_deref(), column);
        assert_eq!(SecretPlace::from_column(column).unwrap(), place);
    }
    for bad in ["header", "query:", "query:a&b", "Path", ""] {
        assert!(SecretPlace::from_column(Some(bad)).is_err(), "{bad:?}");
    }
    let err = SecretPlace::from_column(Some("query:a&b")).unwrap_err();
    assert!(err.to_string().contains("query parameter name"), "{err}");
    let id = StoredId::new("env-1").unwrap();
    let hosts = vec![SecretHost::new("api.telegram.org").unwrap()];
    assert_eq!(
        EnvDraft::secret_in(id.clone(), hosts.clone(), SecretPlace::Path).unwrap(),
        EnvDraft::Secret {
            id: id.clone(),
            hosts,
            place: SecretPlace::Path,
        }
    );
    assert!(EnvDraft::secret_in(id, Vec::new(), SecretPlace::Path).is_err());
}

#[test]
fn a_secret_needs_hosts_and_they_are_sorted_without_repeats() {
    let id = StoredId::new("env-1").unwrap();
    assert!(EnvDraft::secret(id.clone(), Vec::new()).is_err());
    let hosts = ["b.example.com", "a.example.com", "b.example.com"]
        .map(|h| SecretHost::new(h).unwrap())
        .to_vec();
    let draft = EnvDraft::secret(id.clone(), hosts).unwrap();
    assert_eq!(
        draft,
        EnvDraft::Secret {
            id: id.clone(),
            hosts: ["a.example.com", "b.example.com"]
                .map(|h| SecretHost::new(h).unwrap())
                .to_vec(),
            place: SecretPlace::Header,
        }
    );
    let many = (0..=MAX_SECRET_HOSTS)
        .map(|i| SecretHost::new(&format!("h{i}.example.com")).unwrap())
        .collect();
    assert!(EnvDraft::secret(id.clone(), many).is_err());
    let most = (0..MAX_SECRET_HOSTS)
        .map(|i| SecretHost::new(&format!("h{i}.example.com")).unwrap())
        .collect();
    assert!(
        EnvDraft::secret(id, most).is_ok(),
        "the limit itself is allowed"
    );
}

#[test]
fn the_limits_are_the_documented_numbers() {
    assert_eq!(MAX_NAME_LEN, 64);
    assert_eq!(MAX_PLAIN_VALUE_BYTES, 49_152);
    assert_eq!(MAX_SECRET_VALUE_CHARS, 8_192);
    assert_eq!(MAX_ENTRIES_PER_SCOPE, 256);
    assert_eq!(MAX_SCOPE_BYTES, 262_144);
    assert_eq!(MAX_SECRET_HOSTS, 32);
}

#[test]
fn a_host_list_is_checked_sorted_and_shown_as_stored() {
    let listed = SecretHost::list(&["b.example.com", "A.example.com", "b.example.com"]).unwrap();
    let shown: Vec<String> = listed.iter().map(ToString::to_string).collect();
    assert_eq!(shown, ["a.example.com", "b.example.com"]);
    let refused = SecretHost::list(&["a.example.com", "10.0.0.1"]).unwrap_err();
    assert!(refused.to_string().contains("host name"), "{refused}");
    assert!(SecretHost::list::<&str>(&[]).is_err());
}

#[test]
fn a_plain_value_is_any_text_without_a_nul_up_to_the_limit() {
    assert!(EnvDraft::plain("").is_ok());
    assert!(EnvDraft::plain("a b\n'c' $d").is_ok());
    assert!(EnvDraft::plain("a\0b").is_err());
    assert!(EnvDraft::plain(&"x".repeat(MAX_PLAIN_VALUE_BYTES)).is_ok());
    assert!(EnvDraft::plain(&"x".repeat(MAX_PLAIN_VALUE_BYTES + 1)).is_err());
}

#[test]
fn a_secret_value_is_header_text_of_a_sensible_length_and_the_message_never_quotes_it() {
    assert!(check_secret_value("ghp_abcDEF123").is_ok());
    assert!(check_secret_value("pass word with spaces and ünïcode").is_ok());
    assert!(check_secret_value(&"x".repeat(MAX_SECRET_VALUE_CHARS)).is_ok());
    for bad in [
        String::new(),
        "x".repeat(MAX_SECRET_VALUE_CHARS + 1),
        "SECRET\nline2".to_owned(),
        "SECRET\tTAB".to_owned(),
        "SECRET\u{7f}DEL".to_owned(),
        "SECRET\u{85}NEL".to_owned(),
        "SECRET\0NUL".to_owned(),
    ] {
        let err = check_secret_value(&bad).unwrap_err().to_string();
        assert!(!err.contains("SECRET"), "{err}");
        assert!(!bad.is_empty() || err.contains("empty"), "{err}");
    }
}

#[test]
fn scopes_read_aloud_and_name_their_workspace() {
    let ws = WorkspaceId::new("shop").unwrap();
    assert_eq!(EnvScope::Global.to_string(), "the global environment");
    assert_eq!(
        EnvScope::Workspace(ws.clone()).to_string(),
        "shop's environment"
    );
    assert_eq!(EnvScope::Workspace(ws.clone()).workspace(), Some(&ws));
    assert_eq!(EnvScope::Global.column(), None);
}

#[test]
fn names_and_hosts_survive_json_only_when_valid() {
    let name: EnvName = serde_json::from_str("\"NPM_TOKEN\"").unwrap();
    assert_eq!(serde_json::to_string(&name).unwrap(), "\"NPM_TOKEN\"");
    assert!(serde_json::from_str::<EnvName>("\"PATH\"").is_err());
    let host: SecretHost = serde_json::from_str("\"*.example.com\"").unwrap();
    assert_eq!(serde_json::to_string(&host).unwrap(), "\"*.example.com\"");
    assert!(serde_json::from_str::<SecretHost>("\"*.com\"").is_err());
}
