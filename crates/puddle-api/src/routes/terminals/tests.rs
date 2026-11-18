// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_service_that_is_unavailable_or_ended_answers_with_the_status_a_client_can_act_on() {
    let unavailable = ApiError::from(TerminalError::Unavailable("later".into()));
    assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    let ended = ApiError::from(TerminalError::Ended(TerminalEnd::of(EndReason::BootFailed)));
    assert_eq!(ended.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_handed_socket_is_served_and_a_failed_upgrade_returns_without_one() {
    let served = std::cell::Cell::new(None);
    let (handed, socket) = tokio::sync::oneshot::channel();
    handed.send(7_u8).unwrap();
    when_handed(socket, |socket| {
        served.set(Some(socket));
        async {}
    })
    .await;
    assert_eq!(served.take(), Some(7));

    let (handed, socket) = tokio::sync::oneshot::channel::<u8>();
    drop(handed);
    when_handed(socket, |socket| {
        served.set(Some(socket));
        async {}
    })
    .await;
    assert_eq!(served.take(), None, "no upgrade, nothing to serve");
}
