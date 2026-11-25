// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::IdentityId;

use super::*;

fn name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .unwrap()
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn every_kind_of_list_state_problem_note_visibility_and_role_has_its_own_wire_name() {
    let states = [
        (ListState::Ok, "ok"),
        (ListState::Stale, "stale"),
        (ListState::Failed, "failed"),
        (ListState::Unavailable, "unavailable"),
    ];
    for (state, wire) in states {
        assert_eq!(name(&RepoListState::from(state)), wire);
    }
    let problems = [
        (ProblemKind::NotSignedIn, "not_signed_in"),
        (ProblemKind::SourceUnavailable, "source_unavailable"),
        (ProblemKind::TokenRejected, "token_rejected"),
        (ProblemKind::Forbidden, "forbidden"),
        (ProblemKind::NotFound, "not_found"),
        (ProblemKind::RateLimited, "rate_limited"),
        (ProblemKind::Unreachable, "unreachable"),
        (ProblemKind::BadAnswer, "bad_answer"),
        (ProblemKind::Unsupported, "unsupported"),
        (ProblemKind::OrganisationNeeded, "organisation_needed"),
        (ProblemKind::WrongTarget, "wrong_target"),
    ];
    for (kind, wire) in problems {
        assert_eq!(name(&RepoProblemCode::from(kind)), wire);
    }
    let notes = [
        (NoteKind::SsoPartial, "sso_partial"),
        (
            NoteKind::OrganisationsMayBeHidden,
            "organisations_may_be_hidden",
        ),
        (NoteKind::FineGrainedToken, "fine_grained_token"),
        (NoteKind::Truncated, "truncated"),
        (NoteKind::AuthorUnavailable, "author_unavailable"),
        (
            NoteKind::OrganisationsUnavailable,
            "organisations_unavailable",
        ),
        (NoteKind::HostLimitReached, "host_limit_reached"),
    ];
    for (kind, wire) in notes {
        assert_eq!(name(&RepoNoteCode::from(kind)), wire);
    }
    let visibilities = [
        (Visibility::Public, "public"),
        (Visibility::Private, "private"),
        (Visibility::Internal, "internal"),
        (Visibility::Unknown, "unknown"),
    ];
    for (visibility, wire) in visibilities {
        assert_eq!(name(&RepoVisibility::from(visibility)), wire);
    }
    let roles = [
        (Role::Admin, "admin"),
        (Role::Maintain, "maintain"),
        (Role::Write, "write"),
        (Role::Triage, "triage"),
        (Role::Read, "read"),
        (Role::Unknown, "unknown"),
    ];
    for (role, wire) in roles {
        assert_eq!(name(&RepoRole::from(role)), wire);
    }
}

#[test]
fn a_count_past_what_the_wire_holds_saturates() {
    let list = SourceList {
        identity: IdentityId(3),
        credential: usize::MAX,
        host: "github.com".to_owned(),
        organisation: None,
        state: ListState::Ok,
        refreshed_at: None,
        retry_at: None,
        problem: None,
        notes: Vec::new(),
        repos: Arc::from([]),
    };
    let source = RepoSource::from_list(&list);
    assert_eq!(source.credential, u32::MAX);
    assert_eq!(source.repo_count, 0);
}
