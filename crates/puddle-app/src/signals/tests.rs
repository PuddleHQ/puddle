// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use super::*;

fn send_to_self(signal: &str) {
    let pid = std::process::id().to_string();
    let sent = std::process::Command::new("kill")
        .args([signal, &pid])
        .status()
        .is_ok_and(|status| status.success());
    assert!(sent);
}

#[tokio::test]
async fn a_signal_quits_once_and_a_second_one_gives_up() {
    let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel();
    let (give_up_tx, mut give_up_rx) = tokio::sync::mpsc::unbounded_channel();
    let signals = ShutdownSignals::install().unwrap();
    let task = tokio::spawn(quit_on_signals(
        signals,
        move || quit_tx.send(()).unwrap(),
        move || give_up_tx.send(()).unwrap(),
    ));
    send_to_self("-TERM");
    let wait = Duration::from_secs(5);
    assert_eq!(
        tokio::time::timeout(wait, quit_rx.recv()).await,
        Ok(Some(()))
    );
    assert!(
        give_up_rx.try_recv().is_err(),
        "one signal only quits, it does not give up"
    );
    send_to_self("-INT");
    assert_eq!(
        tokio::time::timeout(wait, give_up_rx.recv()).await,
        Ok(Some(()))
    );
    task.await.unwrap();
    assert!(quit_rx.try_recv().is_err(), "quit ran once");
}
