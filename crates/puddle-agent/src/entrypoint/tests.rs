// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use puddle_test_kit::no_event_for;

use super::*;

#[test]
fn an_exit_line_gives_its_status_and_the_end_of_the_output() {
    assert_eq!(parse_exit("b 0", "b"), Some((0, String::new())));
    assert_eq!(parse_exit("b 1 \n", "b"), Some((1, String::new())));
    assert_eq!(
        parse_exit("b 143 Terminated  \nmore", "b"),
        Some((143, "Terminated".into()))
    );
    let long = format!("b 1 {}", "x".repeat(1000));
    assert_eq!(
        parse_exit(&long, "b").map(|(_, t)| t.chars().count()),
        Some(MAX_TAIL_CHARS)
    );
}

#[test]
fn a_line_of_another_boot_or_without_a_status_is_not_an_exit() {
    for bad in [
        "",
        "b",
        "b x out",
        "b 256 out",
        "b -1 out",
        "a 1 out",
        " b 1",
    ] {
        assert_eq!(parse_exit(bad, "b"), None, "{bad:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn each_exit_of_this_boot_is_sent_once() {
    let dir = tempfile::tempdir().unwrap();
    let (exit_file, boot) = (dir.path().join("exit"), dir.path().join("boot_id"));
    std::fs::write(&boot, "b1\n").unwrap();
    let (tx, mut rx) = mpsc::channel(4);
    let task = tokio::spawn(watch(
        EntrypointWatch {
            exit_file: exit_file.clone(),
            boot_id: boot,
            poll: Duration::from_millis(10),
        },
        tx,
    ));
    std::fs::write(&exit_file, "b0 9 last boot\n").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&exit_file, "b1 127 dockerd: not found\n").unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap();
    assert_eq!(
        got,
        Some(AgentMessage::entrypoint_exited(
            127,
            "dockerd: not found".into()
        ))
    );
    // The same exit read again is not sent again; a new one is.
    no_event_for(
        "a second report of the same exit",
        Duration::from_millis(50),
        &mut rx,
    )
    .await;
    std::fs::write(&exit_file, "b1 0 \n").unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap();
    assert_eq!(got, Some(AgentMessage::entrypoint_exited(0, String::new())));
    // The boot hook started it again: the same exit is news.
    std::fs::remove_file(&exit_file).unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&exit_file, "b1 0 \n").unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap();
    assert_eq!(got, Some(AgentMessage::entrypoint_exited(0, String::new())));
    drop(rx);
    // A closed queue ends the watch.
    std::fs::write(&exit_file, "b1 3 again\n").unwrap();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(start_paused = true)]
async fn an_exit_that_finds_the_queue_full_is_sent_at_a_later_tick() {
    let dir = tempfile::tempdir().unwrap();
    let (exit_file, boot) = (dir.path().join("exit"), dir.path().join("boot_id"));
    std::fs::write(&boot, "b1\n").unwrap();
    std::fs::write(&exit_file, "b1 4 late\n").unwrap();
    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(AgentMessage::entrypoint_exited(9, "filler".into()))
        .unwrap();
    let task = tokio::spawn(watch(
        EntrypointWatch {
            exit_file,
            boot_id: boot,
            poll: Duration::from_millis(10),
        },
        tx,
    ));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(
        rx.recv().await,
        Some(AgentMessage::entrypoint_exited(9, "filler".into()))
    );
    let got = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap();
    assert_eq!(got, Some(AgentMessage::entrypoint_exited(4, "late".into())));
    task.abort();
}

#[test]
fn a_boot_id_that_cannot_be_read_is_the_one_the_boot_hook_writes_then() {
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    assert_eq!(boot_id(Path::new("/nonexistent/boot_id")), UNKNOWN_BOOT);
}
