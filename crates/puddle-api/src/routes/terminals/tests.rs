// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

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
