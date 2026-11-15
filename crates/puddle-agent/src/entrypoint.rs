// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
