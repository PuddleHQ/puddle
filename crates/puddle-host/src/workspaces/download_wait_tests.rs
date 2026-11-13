// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn a_download_that_does_not_end_in_time_asks_the_user_to_try_again() {
    let (_state, mut warmth) = watch::channel(Warmth::Warming);
    let err = wait_until_downloaded(&mut warmth, Duration::from_millis(20))
        .await
        .unwrap_err();
    assert!(
        matches!(&err, WorkspaceError::Unavailable(m) if m.contains("still downloading")),
        "{err:?}"
    );
}

#[tokio::test]
async fn a_download_that_ends_while_waiting_lets_the_delete_go_on() {
    let (state, mut warmth) = watch::channel(Warmth::Warming);
    let waiting = wait_until_downloaded(&mut warmth, Duration::from_secs(20));
    state.send_replace(Warmth::Ready);
    assert!(waiting.await.is_ok());
    let (_state, mut failed) = watch::channel(Warmth::Failed);
    assert!(
        wait_until_downloaded(&mut failed, Duration::from_secs(20))
            .await
            .is_ok(),
        "a failed download is the check's to report"
    );
}
