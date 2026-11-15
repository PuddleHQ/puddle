// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::profile::{CLAUDE, Role, Scope};

fn field(path: &str, kind: FieldKind) -> BodyField {
    BodyField {
        name: path.rsplit('.').next().unwrap_or(path).to_owned(),
        path: path.to_owned(),
        kind,
    }
}

#[test]
fn every_problem_a_tool_can_be_refused_for_says_its_own_reason() {
    for (problem, reason) in [
        (
            Problem::StoreUnavailable,
            "credential store is not available",
        ),
        (Problem::UnexpectedAnswer, "in a form Puddle does not read"),
        (Problem::UnusableToken, "cannot give a stand-in for"),
        (Problem::BoundToken, "ties its token to a key"),
        (Problem::UnnamedToken, "field Puddle does not know"),
        (Problem::Unreadable, "could not keep it"),
    ] {
        let refusal = refusal_for(&CLAUDE, problem);
        assert_eq!(refusal.status(), 502);
        assert!(refusal.message().contains(reason), "{problem:?}");
    }
}

#[test]
fn a_field_name_says_it_holds_a_credential_by_each_of_its_endings_and_words() {
    for name in [
        "id_token",
        "Client_Secret",
        "password",
        "key",
        "api_key",
        "apikey",
        "signing_secret_value",
        "my_password_hash",
    ] {
        assert!(token_like(name), "{name}");
    }
    for name in [
        "token_type",
        "token_expires_in",
        "token_expires_at",
        "token_url",
        "token_uri",
        "token_endpoint",
        "token_scope",
        "token_label",
        "scope",
        "keyboard",
        "account",
    ] {
        assert!(!token_like(name), "{name}");
    }
}

#[test]
fn a_field_named_by_a_dotted_path_is_named_at_that_path_only() {
    static NESTED: [Field; 1] = [Field {
        path: "data.token",
        role: Role::Access,
        resource: None,
        prefixes: &[],
        keep_last: 0,
        stand_in: crate::profile::StandInKind::Shaped,
        scope: Scope::Api,
        audience: Audience::Any,
        keep: Keep::Stored,
        only_prefixed: false,
    }];
    let mut endpoint = CLAUDE.endpoints[0];
    endpoint.fields = &NESTED;
    let long = FieldKind::Text(MIN_TOKEN_LEN);
    let holds = |fields: Vec<BodyField>| holds_unnamed_token(&endpoint, &fields);
    assert!(!holds(vec![field("data.token", long)]));
    assert!(holds(vec![
        field("data.token", long),
        field("data.token", long)
    ]));
    assert!(holds(vec![field("data.token", FieldKind::Structured)]));
    assert!(holds(vec![field("other.token", long)]));
    assert!(holds(vec![field("token", long)]));
}

#[test]
fn a_credential_puddle_would_not_replace_is_found_in_the_answers_fields() {
    let endpoint = &CLAUDE.endpoints[0];
    let long = FieldKind::Text(MIN_TOKEN_LEN);
    let short = FieldKind::Text(MIN_TOKEN_LEN - 1);
    let holds = |fields: Vec<BodyField>| holds_unnamed_token(endpoint, &fields);
    assert!(!holds(vec![field("access_token", long)]));
    assert!(holds(vec![
        field("access_token", long),
        field("access_token", long)
    ]));
    assert!(holds(vec![field("access_token", FieldKind::Structured)]));
    assert!(holds(vec![field("data.access_token", long)]));
    assert!(holds(vec![field("id_token", long)]));
    assert!(!holds(vec![field("id_token", short)]));
    assert!(!holds(vec![field("id_token", FieldKind::Empty)]));
    assert!(!holds(vec![field("note", long)]));
    assert!(!holds(vec![
        field("access_token", long),
        field("refresh_token", long)
    ]));
}
