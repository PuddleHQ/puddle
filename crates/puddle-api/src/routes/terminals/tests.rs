// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_service_that_is_unavailable_or_ended_answers_with_the_status_a_client_can_act_on() {
    let unavailable = ApiError::from(TerminalError::Unavailable("later".into()));
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ended = ApiError::from(TerminalError::Ended(TerminalEnd::of(EndReason::BootFailed)));
    assert_eq!(ended.status(), StatusCode::CONFLICT);
}
