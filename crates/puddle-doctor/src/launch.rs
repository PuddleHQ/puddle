// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs a program with a time limit and keeps the start of its output and the end of its errors.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::facts::ProcessOutcome;

/// How many lines of a program's standard error a report keeps.
pub const TAIL_LINES: usize = 8;

/// The most output kept per stream; the rest is read and dropped so the program never blocks.
const KEEP_BYTES: usize = 64 * 1024;

/// How often a running program is checked for exit.
const POLL: Duration = Duration::from_millis(20);

/// How long to wait for the output pipes after the program exited: a grandchild that inherited
/// them may keep them open.
const DRAIN: Duration = Duration::from_secs(2);

/// Runs `command` (stdin closed, output captured) and kills it after `limit`.
pub(crate) fn run(command: &mut Command, limit: Duration) -> ProcessOutcome {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ProcessOutcome::SpawnFailed {
                os_error: e.raw_os_error(),
                detail: e.to_string(),
            };
        }
    };
    let stdout = Capture::start(child.stdout.take());
    let stderr = Capture::start(child.stderr.take());
    let status = wait(&mut child, start + limit);
    let elapsed = start.elapsed();
    let Some(status) = status else {
        kill(&mut child);
        return ProcessOutcome::TimedOut { after: limit };
    };
    let drain_until = Instant::now() + DRAIN;
    ProcessOutcome::Exited {
        code: status.code(),
        stdout_first_line: first_line(&stdout.text(drain_until)),
        stderr_tail: tail(&stderr.text(drain_until), TAIL_LINES),
        elapsed,
    }
}

/// Waits until the child exits or `deadline` passes (`None`).
fn wait(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            // The handle is unusable; treat it like a hang so the caller kills it.
            Err(_) => return None,
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::sleep(POLL.min(deadline - now));
    }
}

fn kill(child: &mut Child) {
    // Already gone is fine; there is nothing else to do about a failed kill.
    let _ignored = child.kill();
    let _reaped = child.wait();
}

/// One output stream read on its own thread into a bounded buffer.
struct Capture {
    buf: Arc<Mutex<Vec<u8>>>,
    done: Arc<Mutex<bool>>,
}

impl Capture {
    fn start(stream: Option<impl Read + Send + 'static>) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let done = Arc::new(Mutex::new(stream.is_none()));
        if let Some(mut stream) = stream {
            let (b, d) = (Arc::clone(&buf), Arc::clone(&done));
            thread::spawn(move || {
                let mut chunk = [0_u8; 4096];
                while let Ok(n @ 1..) = stream.read(&mut chunk) {
                    let mut kept = b.lock().unwrap_or_else(PoisonError::into_inner);
                    let room = KEEP_BYTES.saturating_sub(kept.len());
                    kept.extend(chunk.iter().take(n.min(room)));
                }
                *d.lock().unwrap_or_else(PoisonError::into_inner) = true;
            });
        }
        Self { buf, done }
    }

    /// What was read, once the stream ended or `until` passed.
    fn text(&self, until: Instant) -> String {
        while !*self.done.lock().unwrap_or_else(PoisonError::into_inner) && Instant::now() < until {
            thread::sleep(POLL);
        }
        let bytes = self.buf.lock().unwrap_or_else(PoisonError::into_inner);
        clean(&String::from_utf8_lossy(&bytes))
    }
}

/// Removes ANSI escape sequences and control characters other than newlines and tabs.
fn clean(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // Parameters and intermediates, then one final byte in '@'..='~'.
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
        } else if c == '\n' || c == '\t' || !c.is_control() {
            out.push(c);
        }
    }
    out
}

fn first_line(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .to_owned()
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let skip = lines.len().saturating_sub(n);
    lines.into_iter().skip(skip).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests;
