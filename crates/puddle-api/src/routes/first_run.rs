// SPDX-License-Identifier: GPL-3.0-or-later
//! The first-run flow's state: shown until the user has been through it or skipped it, kept in
//! the global settings document with the user's other settings.

use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use puddle_settings::UnixMillis;

use crate::ApiErrorBody;
use crate::dev_cert::DevCertService;
use crate::error::{ApiError, blocking};
use crate::routes::AppState;
use crate::routes::settings::load_global;
use crate::wire::{DevCertTrustResult, DevCertificate, FirstRun, FirstRunRequest};

/// How long the button waits for the system's confirmation: the limit a workspace start puts on
/// it. The route lives in the slow router, whose time limit is longer, so this answers first.
pub(crate) const TRUST_WAIT: Duration = Duration::from_secs(120);

fn view(first_run: &puddle_settings::FirstRun, dev_certificate: DevCertificate) -> FirstRun {
    FirstRun {
        completed: first_run.is_completed(),
        completed_at: first_run.completed_at.map(|at| at.0),
        dev_certificate,
    }
}

/// Whether the first-run flow has been through, and what its certificates step shows.
#[utoipa::path(
    get,
    path = "/api/first-run",
    tag = "first-run",
    responses(
        (status = OK, description = "the flow's state", body = FirstRun),
        (status = CONFLICT, description = "settings written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_first_run(
    State(state): State<AppState>,
) -> Result<Json<FirstRun>, ApiError> {
    let _lock = state.settings_lock.lock().await;
    let loaded = blocking(move || {
        let loaded = load_global(state.settings.as_ref())?;
        Ok(view(&loaded.settings.first_run, state.dev_cert.status()))
    })
    .await?;
    Ok(Json(loaded))
}

/// Records that the first-run flow was finished or skipped (`true`), or makes it run again at the
/// next start (`false`). Doing it twice keeps the first time.
#[utoipa::path(
    put,
    path = "/api/first-run",
    tag = "first-run",
    request_body = FirstRunRequest,
    responses(
        (status = OK, description = "the flow's state after the change", body = FirstRun),
        (status = CONFLICT, description = "settings written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_first_run(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<FirstRunRequest>,
) -> Result<Json<FirstRun>, ApiError> {
    let now = state.clock.now_ms();
    let _lock = state.settings_lock.lock().await;
    let first_run = blocking(move || {
        let repo = state.settings.as_ref();
        let mut loaded = load_global(repo)?;
        let first_run = &mut loaded.settings.first_run;
        match (body.completed, first_run.completed_at) {
            (true, None) => first_run.completed_at = Some(UnixMillis(now)),
            (false, Some(_)) => first_run.completed_at = None,
            _ => return Ok(view(first_run, state.dev_cert.status())),
        }
        repo.save_global(loaded.settings.to_document())?;
        tracing::info!(completed = body.completed, "first-run state changed");
        Ok(view(&loaded.settings.first_run, state.dev_cert.status()))
    })
    .await?;
    Ok(Json(first_run))
}

/// Makes Puddle's development certificate if it has none and asks the system to trust it. On
/// Windows that opens a confirmation the user answers; this waits up to two minutes for it. A
/// refusal or a failure is an answer (`trust_declined`, `trust_failed`), not an error.
#[utoipa::path(
    post,
    path = "/api/dev-certificate/trust",
    tag = "first-run",
    responses(
        (status = OK, description = "where the certificate stands after the attempt", body = DevCertTrustResult)
    )
)]
pub(crate) async fn trust_dev_certificate(
    State(state): State<AppState>,
) -> Result<Json<DevCertTrustResult>, ApiError> {
    Ok(Json(trust_within(state.dev_cert, TRUST_WAIT).await))
}

/// The service's answer to a trust request, or the state as it stands (`waiting`) when the
/// system's confirmation is still unanswered after `wait`. The confirmation stays open and a
/// later answer shows up in the next read of the state.
async fn trust_within(service: Arc<dyn DevCertService>, wait: Duration) -> DevCertTrustResult {
    let asking = service.clone();
    let work = tokio::task::spawn_blocking(move || asking.trust());
    match tokio::time::timeout(wait, work).await {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            tracing::warn!(%error, "trusting the development certificate failed");
            DevCertTrustResult {
                dev_certificate: DevCertificate::TrustFailed,
                detail: Some("Puddle's attempt to trust it stopped unexpectedly.".to_owned()),
                waiting: false,
            }
        }
        Err(_) => DevCertTrustResult {
            dev_certificate: tokio::task::spawn_blocking(move || service.status())
                .await
                .unwrap_or(DevCertificate::NotChecked),
            detail: None,
            waiting: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeDevCert;

    /// Takes `delay` to answer a trust request, and panics when `panic` is set.
    struct Slow {
        delay: Duration,
        panic: bool,
    }

    impl DevCertService for Slow {
        fn status(&self) -> DevCertificate {
            DevCertificate::NeedsTrust
        }
        fn trust(&self) -> DevCertTrustResult {
            // Simulated slow confirmation, longer than the wait under test.
            std::thread::sleep(self.delay);
            assert!(!self.panic, "the store blew up");
            DevCertTrustResult {
                dev_certificate: DevCertificate::Trusted,
                detail: None,
                waiting: false,
            }
        }
    }

    #[tokio::test]
    async fn an_answer_in_time_is_passed_on() {
        let fake = Arc::new(FakeDevCert::new(DevCertificate::NeedsTrust));
        let result = trust_within(fake, Duration::from_secs(30)).await;
        assert_eq!(result.dev_certificate, DevCertificate::Trusted);
        assert_eq!(result.detail, None);
    }

    #[tokio::test]
    async fn an_unanswered_confirmation_leaves_the_state_as_it_is_and_says_so() {
        let slow = Arc::new(Slow {
            delay: Duration::from_millis(300),
            panic: false,
        });
        let result = trust_within(slow, Duration::from_millis(20)).await;
        assert_eq!(result.dev_certificate, DevCertificate::NeedsTrust);
        assert!(result.waiting);
        assert_eq!(result.detail, None);
    }

    #[tokio::test]
    async fn a_service_that_panics_is_a_failed_attempt_not_a_broken_route() {
        let broken = Arc::new(Slow {
            delay: Duration::ZERO,
            panic: true,
        });
        let result = trust_within(broken, Duration::from_secs(30)).await;
        assert_eq!(result.dev_certificate, DevCertificate::TrustFailed);
        assert!(result.detail.is_some());
    }
}
