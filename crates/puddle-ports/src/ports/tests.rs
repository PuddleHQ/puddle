// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[tokio::test]
async fn ended_sessions_wait_for_ever_once_the_ports_are_gone() {
    let (tx, rx) = tokio::sync::broadcast::channel(4);
    let mut ended = EndedSessions(rx);
    drop(tx);
    let waited = tokio::time::timeout(Duration::from_millis(50), ended.next()).await;
    assert!(waited.is_err());
}
