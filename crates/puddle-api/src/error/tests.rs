// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::{PendingId, RuleId};

use super::*;

#[test]
fn ports_refusals_map_to_the_status_a_client_can_act_on() {
    for (err, status) in [
        (
            PortsServiceError::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            PortsServiceError::NotRunning("x".into()),
            StatusCode::CONFLICT,
        ),
        (PortsServiceError::Taken("x".into()), StatusCode::CONFLICT),
        (
            PortsServiceError::Invalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            PortsServiceError::NotForwarded("x".into()),
            StatusCode::NOT_FOUND,
        ),
        (
            PortsServiceError::Failed("the pinned forwards could not be saved".into()),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
    ] {
        assert_eq!(ApiError::from(err.clone()).status(), status, "{err}");
    }
}

#[test]
fn store_errors_map_to_client_statuses() {
    let cases = [
        (
            StoreError::UnknownPending(PendingId(1)),
            StatusCode::NOT_FOUND,
        ),
        (StoreError::UnknownRule(RuleId(1)), StatusCode::NOT_FOUND),
        (StoreError::UnknownRequestRule(1), StatusCode::NOT_FOUND),
        (
            StoreError::RequestRuleInvalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            StoreError::UnknownRuleSet("user:1".into()),
            StatusCode::NOT_FOUND,
        ),
        (
            StoreError::RuleSetOff {
                set: "user:1".into(),
                workspace: "a".into(),
            },
            StatusCode::CONFLICT,
        ),
        (
            StoreError::RuleSetHidden("builtin:x".into()),
            StatusCode::CONFLICT,
        ),
        (StoreError::NotSwitchable, StatusCode::UNPROCESSABLE_ENTITY),
        (
            StoreError::RuleSetName("taken".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            StoreError::PendingNotOpen {
                id: PendingId(1),
                state: puddle_store::PendingState::Allowed,
            },
            StatusCode::CONFLICT,
        ),
        (
            StoreError::ExpiryNotInFuture,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (StoreError::SystemActor, StatusCode::INTERNAL_SERVER_ERROR),
        (
            StoreError::UnknownIdentity(puddle_store::IdentityId(1)),
            StatusCode::NOT_FOUND,
        ),
        (StoreError::UnknownRepo(1), StatusCode::NOT_FOUND),
        (
            StoreError::UnknownEnv {
                scope: "the global environment".into(),
                name: "A".into(),
            },
            StatusCode::NOT_FOUND,
        ),
        (
            StoreError::EnvInvalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            StoreError::ShellInitInvalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            StoreError::IdentityInvalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            StoreError::IdentityLabelTaken("x".into()),
            StatusCode::CONFLICT,
        ),
        (
            StoreError::IdentityAttached { label: "x".into() },
            StatusCode::CONFLICT,
        ),
        (StoreError::RepoListed("x".into()), StatusCode::CONFLICT),
    ];
    for (err, status) in cases {
        assert_eq!(ApiError::from(err).status(), status);
    }
}

#[test]
fn settings_errors_map_newer_schema_to_a_conflict() {
    let newer = ApiError::from(SettingsError::NewerSchema {
        kind: "global",
        found: 9,
        supported: 1,
    });
    assert_eq!(newer.status(), StatusCode::CONFLICT);
    assert_eq!(newer.code(), ErrorCode::NewerSettings);
    let other = ApiError::from(SettingsError::NotAnObject { kind: "global" });
    assert_eq!(other.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let damaged =
        ApiError::settings_unreadable("the global settings", &"global settings must be an object");
    assert_eq!(damaged.status(), StatusCode::CONFLICT);
    assert_eq!(damaged.code(), ErrorCode::SettingsUnreadable);
    let repo = ApiError::from(SettingsRepoError::new("disk full"));
    assert_eq!(repo.code(), ErrorCode::Internal);
}

#[test]
fn workspace_errors_map_to_client_statuses() {
    let cases = [
        (
            WorkspaceError::NotFound("x".into()),
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
        ),
        (
            WorkspaceError::Conflict("x".into()),
            StatusCode::CONFLICT,
            ErrorCode::Conflict,
        ),
        (
            WorkspaceError::Invalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            ErrorCode::Invalid,
        ),
        (
            WorkspaceError::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
        ),
        (
            WorkspaceError::Internal("disk path /secret".into()),
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
        ),
    ];
    for (err, status, code) in cases {
        let api = ApiError::from(err);
        assert_eq!(api.status(), status);
        assert_eq!(api.code(), code);
        assert!(!api.body.message.contains("secret"));
    }
}

#[test]
fn doctor_errors_map_to_statuses_and_keep_internals_out_of_the_message() {
    let missing = ApiError::from(DoctorError::Unavailable("not in this build".into()));
    assert_eq!(missing.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(missing.code(), ErrorCode::Unavailable);
    let crashed = ApiError::from(DoctorError::Crashed("panic at /secret/path".into()));
    assert_eq!(crashed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(crashed.code(), ErrorCode::Internal);
    assert!(!crashed.body.message.contains("secret"));
}

#[tokio::test]
async fn a_panicking_blocking_task_is_a_500() {
    let err = blocking::<(), _>(|| std::panic::resume_unwind(Box::new("boom")))
        .await
        .unwrap_err();
    assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let ok = blocking(|| Ok(7)).await.unwrap();
    assert_eq!(ok, 7);
}
