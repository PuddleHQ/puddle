// SPDX-License-Identifier: GPL-3.0-or-later
//! Watches for the image entrypoint's exit and reports it to the host.
//!
//! The boot hook starts the entrypoint under a wrapper that waits for it and then writes one line,
//! `<boot id> <status> <end of output>`, to a file (renamed into place). The agent is not the
//! entrypoint's parent, so it cannot wait for it itself; it reads that file once a second and sends
//! each exit of this boot once ([`AgentMessage::EntrypointExited`]).
//!
//! ```
//! use puddle_agent::entrypoint::parse_exit;
//! assert_eq!(parse_exit("b1 127 sh: x: not found\n", "b1"), Some((127, "sh: x: not found".into())));
//! assert_eq!(parse_exit("b0 1 from the last boot", "b1"), None);
//! ```

use std::io::Read as _;
use std::path::Path;

use puddle_agent_proto::AgentMessage;
use puddle_agent_proto::host::MAX_TAIL_CHARS;
use tokio::sync::mpsc;

use crate::config::EntrypointWatch;

/// Most bytes read from the exit file (the wrapper writes a few hundred).
const MAX_FILE: u64 = 4096;

/// What `boot.sh` writes when it cannot read the boot id; the agent says the same then.
const UNKNOWN_BOOT: &str = "unknown";

/// The status and end of output in an exit line, when it is of the boot `boot_id`. The end of
/// output is cut to what the host takes.
#[must_use]
pub fn parse_exit(line: &str, boot_id: &str) -> Option<(u8, String)> {
    let line = line.lines().next()?;
    let (boot, rest) = line.split_once(' ')?;
    if boot != boot_id {
        return None;
    }
    let (status, tail) = rest.split_once(' ').unwrap_or((rest, ""));
    let status = status.parse().ok()?;
    Some((status, tail.trim().chars().take(MAX_TAIL_CHARS).collect()))
}

fn read_bounded(path: &Path) -> Option<String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_FILE)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

fn boot_id(path: &Path) -> String {
    read_bounded(path)
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| UNKNOWN_BOOT.to_owned())
}

/// Reads `watch.exit_file` every `watch.poll` and sends each new exit of this boot to `out`.
/// Returns when `out` closes.
pub async fn watch(watch: EntrypointWatch, out: mpsc::Sender<AgentMessage>) {
    let boot = boot_id(&watch.boot_id);
    let mut reported: Option<(u8, String)> = None;
    let mut ticks = tokio::time::interval(watch.poll);
    loop {
        ticks.tick().await;
        let Some(line) = read_bounded(&watch.exit_file) else {
            // The boot hook removes the file when it starts the entrypoint again: the next exit
            // is news even if it reads the same.
            reported = None;
            continue;
        };
        let exit = parse_exit(&line, &boot);
        let Some(exit) = exit.filter(|e| reported.as_ref() != Some(e)) else {
            continue;
        };
        let (status, tail) = exit.clone();
        match out.try_send(AgentMessage::entrypoint_exited(status, tail)) {
            Ok(()) => {
                tracing::info!(status, "the image's entrypoint exited; the host is told");
                reported = Some(exit);
            }
            // Tried again at the next tick.
            Err(mpsc::error::TrySendError::Full(_)) => {}
            Err(mpsc::error::TrySendError::Closed(_)) => return,
        }
    }
}

#[cfg(test)]
mod tests {
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
        assert_eq!(boot_id(Path::new("/nonexistent/boot_id")), UNKNOWN_BOOT);
    }
}
