// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicU32, Ordering};
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

async fn until(count: &AtomicU32, at_least: u32) {
    let ran = tokio::time::timeout(Duration::from_secs(5), async {
        while count.load(Ordering::SeqCst) < at_least {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(ran.is_ok(), "the callback ran");
}

#[tokio::test]
async fn a_signal_quits_once_and_a_second_one_gives_up() {
    static QUIT: AtomicU32 = AtomicU32::new(0);
    static GAVE_UP: AtomicU32 = AtomicU32::new(0);
    let signals = ShutdownSignals::install().unwrap();
    let task = tokio::spawn(quit_on_signals(
        signals,
        || {
            QUIT.fetch_add(1, Ordering::SeqCst);
        },
        || {
            GAVE_UP.fetch_add(1, Ordering::SeqCst);
        },
    ));
    send_to_self("-TERM");
    until(&QUIT, 1).await;
    assert_eq!(GAVE_UP.load(Ordering::SeqCst), 0, "one signal only quits");
    send_to_self("-INT");
    until(&GAVE_UP, 1).await;
    task.await.unwrap();
    assert_eq!(QUIT.load(Ordering::SeqCst), 1);
}
