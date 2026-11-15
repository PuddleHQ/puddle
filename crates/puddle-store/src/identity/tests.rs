// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use proptest::prelude::*;
use puddle_secrets::{AccountName, StoredId, TokenScope};

#[test]
fn an_identity_id_shows_as_its_number() {
    assert_eq!(IdentityId(42).to_string(), "42");
}

const HOSTS: [&str; 2] = ["github.com", "dev.azure.com"];
const OWNERS: [&str; 4] = ["acme", "acme-labs", "me", "other"];

fn binding(host: &str, owners: &[&str], rest: bool) -> CredentialBinding {
    let owners = owners.iter().map(|o| Owner::new(o).unwrap()).collect();
    CredentialBinding::new(
        &HostName::new(host).unwrap(),
        SourceSpec::Gh {
            host: HostName::new(host).unwrap(),
            account: AccountName::new("me").unwrap(),
        },
        Coverage::new(owners, rest).unwrap(),
    )
    .unwrap()
}

fn identity(id: i64, label: &str, credentials: Vec<CredentialBinding>) -> Identity {
    Identity {
        id: IdentityId(id),
        label: label.to_owned(),
        author: Author::new("Me", "me@example.com").unwrap(),
        credentials,
        signing: Signing::None,
        is_default: false,
        created_at: 0,
        changed_at: 0,
    }
}

#[test]
fn exact_owner_beats_rest_of_host() {
    let work = identity(
        1,
        "Work",
        vec![binding("github.com", &["acme", "acme-labs"], false)],
    );
    let personal = identity(2, "Personal", vec![binding("github.com", &[], true)]);
    let both = [work, personal];
    for (owner, who, exact) in [("acme", 1, true), ("ACME-Labs", 1, true), ("me", 2, false)] {
        assert!(
            matches!(
                resolve(&both, "GitHub.com", owner),
                CredentialChoice::Covered { identity, exact: e, .. }
                    if (identity.id, e) == (IdentityId(who), exact)
            ),
            "{owner}"
        );
    }
    assert_eq!(
        resolve(&both, "dev.azure.com", "acme"),
        CredentialChoice::Uncovered
    );
}

#[test]
fn a_collision_names_both_and_the_place() {
    let work = identity(1, "Work", vec![binding("github.com", &["acme"], false)]);
    let personal = identity(
        2,
        "Personal",
        vec![binding("github.com", &["Acme", "me"], false)],
    );
    let c = check_attachable(std::slice::from_ref(&work), &personal).unwrap_err();
    assert_eq!(
        c.to_string(),
        "Work and Personal both cover github.com/acme; narrow one"
    );
    let rest = identity(3, "Rest", vec![binding("github.com", &[], true)]);
    let rest2 = identity(4, "Other rest", vec![binding("github.com", &["x"], true)]);
    let c = check_attachable(&[rest], &rest2).unwrap_err();
    assert_eq!(
        c.to_string(),
        "Rest and Other rest both cover the rest of github.com; narrow one"
    );
    // Another host, or an exact owner against the rest of the host, is no collision.
    let azure = identity(5, "Azure", vec![binding("dev.azure.com", &["acme"], true)]);
    assert!(check_attachable(std::slice::from_ref(&work), &azure).is_ok());
    let rest = identity(6, "Rest", vec![binding("github.com", &[], true)]);
    assert!(check_attachable(std::slice::from_ref(&work), &rest).is_ok());
}

#[test]
fn an_identity_does_not_collide_with_itself() {
    let work = identity(1, "Work", vec![binding("github.com", &["acme"], false)]);
    assert!(check_attachable(std::slice::from_ref(&work), &work).is_ok());
}

#[test]
fn a_hand_edited_overlap_is_ambiguous_never_a_guess() {
    let a = identity(1, "A", vec![binding("github.com", &["acme"], false)]);
    let b = identity(2, "B", vec![binding("github.com", &["acme"], false)]);
    assert_eq!(
        resolve(&[a, b], "github.com", "acme"),
        CredentialChoice::Ambiguous(vec![IdentityId(1), IdentityId(2)])
    );
}

#[test]
fn a_credential_for_a_port_other_than_443_is_refused() {
    let own = |host: &str| {
        let host = HostName::new(host).unwrap();
        let gh = SourceSpec::Gh {
            host: host.clone(),
            account: AccountName::new("me").unwrap(),
        };
        CredentialBinding::new(&host, gh, Coverage::new(BTreeSet::new(), true).unwrap())
    };
    let refused = own("ghe.example:8443").unwrap_err().to_string();
    assert!(refused.contains("port 8443"), "{refused}");
    assert!(refused.contains("never be used"), "{refused}");
    assert!(own("ghe.example:443").is_ok());
    assert!(own("ghe.example").is_ok());
}

#[test]
fn a_stored_binding_for_such_a_port_still_reads() {
    let stored = r#"{"host":"ghe.example:8443","source":{"kind":"gh","host":"ghe.example:8443","account":"me"},"covers":{"owners":[],"rest_of_host":true}}"#;
    let read: Result<CredentialBinding, _> = serde_json::from_str(stored);
    assert!(read.is_ok(), "{read:?}");
}

#[test]
fn bindings_are_checked() {
    let host = HostName::new("dev.azure.com").unwrap();
    let stored = SourceSpec::Stored {
        id: StoredId::new("t1").unwrap(),
        scope: TokenScope {
            host: host.clone(),
            org: Some(OrgName::new("Contoso").unwrap()),
        },
    };
    let own = Coverage::new([Owner::new("contoso").unwrap()].into(), false).unwrap();
    assert!(CredentialBinding::new(&host, stored.clone(), own).is_ok());
    let other = Coverage::new([Owner::new("acme").unwrap()].into(), false).unwrap();
    assert!(CredentialBinding::new(&host, stored.clone(), other).is_err());
    let rest = Coverage::new(BTreeSet::new(), true).unwrap();
    assert!(CredentialBinding::new(&host, stored, rest.clone()).is_err());
    let gh = SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new("me").unwrap(),
    };
    assert!(CredentialBinding::new(&host, gh, rest).is_err());
    assert!(Coverage::new(BTreeSet::new(), false).is_err());
}

#[test]
fn author_and_label_are_checked() {
    assert!(Author::new("Me", "me@example.com").is_ok());
    assert!(Author::new(&"n".repeat(101), "me@example.com").is_err());
    for (name, email) in [
        ("", "a@b"),
        ("Me", "nobody"),
        ("Me", "a b@c"),
        ("Me", "@c"),
        ("M\ne", "a@b"),
        ("Me", "a@b@c"),
    ] {
        assert!(Author::new(name, email).is_err(), "{name:?} {email:?}");
    }
    let draft = |label: &str, credentials| IdentityDraft {
        label: label.into(),
        author: Author::new("Me", "me@example.com").unwrap(),
        credentials,
    };
    assert!(draft(" ", vec![]).checked().is_err());
    assert_eq!(draft(" Work ", vec![]).checked().unwrap().label, "Work");
    let twice = vec![
        binding("github.com", &["acme"], false),
        binding("github.com", &["acme"], false),
    ];
    assert!(draft("x", twice).checked().is_err());
    let apart = vec![
        binding("github.com", &["acme"], false),
        binding("github.com", &["other"], false),
    ];
    assert_eq!(draft("x", apart).checked().unwrap().credentials.len(), 2);
    let many = vec![binding("github.com", &["a"], false); MAX_CREDENTIALS + 1];
    assert!(draft("x", many).checked().is_err());
}

#[test]
fn a_label_has_a_length_limit_and_keeps_out_what_git_drops() {
    let draft = |label: &str, credentials| IdentityDraft {
        label: label.into(),
        author: Author::new("Me", "me@example.com").unwrap(),
        credentials,
    };
    assert!(draft(&"l".repeat(MAX_LABEL), vec![]).checked().is_ok());
    let err = draft(&"l".repeat(MAX_LABEL + 1), vec![])
        .checked()
        .unwrap_err();
    assert!(err.to_string().contains("longer than 64"), "{err}");
    for bad in ["a<b", "a>b", "a\tb", "a\u{7f}b"] {
        let err = draft(bad, vec![]).checked().unwrap_err();
        assert!(
            err.to_string().contains("git does not keep"),
            "{bad:?}: {err}"
        );
    }
    // Only the author's name uses the same rule: a character in the middle is as bad as at the end.
    assert!(Author::new("Me<", "me@example.com").is_err());
    assert!(Author::new("Me>", "me@example.com").is_err());
}

#[test]
fn an_identity_holds_exactly_the_most_credentials() {
    let draft = |n: usize| IdentityDraft {
        label: "x".into(),
        author: Author::new("Me", "me@example.com").unwrap(),
        credentials: (0..n)
            .map(|i| binding("github.com", &[&format!("owner{i}")], false))
            .collect(),
    };
    assert_eq!(
        draft(MAX_CREDENTIALS).checked().unwrap().credentials.len(),
        MAX_CREDENTIALS
    );
    let err = draft(MAX_CREDENTIALS + 1).checked().unwrap_err();
    assert!(err.to_string().contains("at most 16"), "{err}");
}

#[test]
fn coverage_survives_json_and_rejects_nonsense() {
    let c = binding("github.com", &["Acme"], true);
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains(r#""owners":["acme"]"#), "{json}");
    assert_eq!(serde_json::from_str::<CredentialBinding>(&json).unwrap(), c);
    let empty = json.replace(
        r#""owners":["acme"],"rest_of_host":true"#,
        r#""owners":[],"rest_of_host":false"#,
    );
    assert!(serde_json::from_str::<CredentialBinding>(&empty).is_err());
}

fn arb_binding() -> impl Strategy<Value = CredentialBinding> {
    (
        0..HOSTS.len(),
        proptest::collection::vec(0..OWNERS.len(), 0..3),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_filter_map("empty coverage", |(h, owners, rest, upper)| {
            let names: Vec<String> = owners
                .iter()
                .map(|&i| {
                    if upper {
                        OWNERS[i].to_ascii_uppercase()
                    } else {
                        OWNERS[i].to_owned()
                    }
                })
                .collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            (!refs.is_empty() || rest).then(|| binding(HOSTS[h], &refs, rest))
        })
}

fn arb_identities() -> impl Strategy<Value = Vec<Identity>> {
    proptest::collection::vec(proptest::collection::vec(arb_binding(), 1..3), 1..6).prop_map(
        |all| {
            all.into_iter()
                .enumerate()
                .map(|(i, creds)| identity(i64::try_from(i).unwrap() + 1, &format!("I{i}"), creds))
                .collect()
        },
    )
}

proptest! {
    /// Attach greedily the way the store does; then no request has two candidates, and the
    /// winner is the identity that names the owner, else the one that covers the rest.
    #[test]
    fn no_two_attached_identities_match_one_request(candidates in arb_identities()) {
        let mut attached: Vec<Identity> = Vec::new();
        for candidate in candidates {
            if candidate.credentials.iter().enumerate().any(|(i, a)| {
                candidate.credentials.iter().skip(i + 1).any(|b| a.collides_with(b).is_some())
            }) {
                continue;
            }
            if check_attachable(&attached, &candidate).is_ok() {
                attached.push(candidate);
            }
        }
        for host in HOSTS {
            for owner in OWNERS.iter().map(|o| o.to_ascii_uppercase()).chain(["nobody".to_owned()]) {
                let low = owner.to_ascii_lowercase();
                let names: Vec<_> = attached.iter().filter(|i| i.credentials.iter().any(|b|
                    b.host.as_str() == host && b.covers.owners.iter().any(|o| o.as_str() == low))).collect();
                let rests: Vec<_> = attached.iter().filter(|i| i.credentials.iter().any(|b|
                    b.host.as_str() == host && b.covers.rest_of_host)).collect();
                prop_assert!(names.len() <= 1 && rests.len() <= 1);
                match resolve(&attached, host, &owner) {
                    CredentialChoice::Covered { identity, exact, binding } => {
                        prop_assert_eq!(exact, !names.is_empty());
                        let expected = if exact { names[0] } else { rests[0] };
                        prop_assert_eq!(identity.id, expected.id);
                        prop_assert_eq!(binding.host.as_str(), host);
                    }
                    CredentialChoice::Uncovered => prop_assert!(names.is_empty() && rests.is_empty()),
                    other => prop_assert!(false, "ambiguous: {other:?}"),
                }
            }
        }
    }

    #[test]
    fn collisions_are_symmetric(identities in arb_identities()) {
        for a in &identities {
            for b in &identities {
                prop_assert_eq!(collision(a, b).is_some(), collision(b, a).is_some());
            }
        }
    }
}
