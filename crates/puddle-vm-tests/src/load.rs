// SPDX-License-Identifier: AGPL-3.0-or-later
//! The synthetic vsock load and its judge: one `curl` runs N parallel downloads of the same URL, and
//! the lines it writes are checked against the bar (every transfer completes with every byte, none
//! goes [`STALL_SECS`] seconds without progress). Test code only.
//!
//! curl is the stall detector: `--speed-limit 1 --speed-time 5` aborts a transfer whose rate stays
//! under one byte a second for five seconds (exit 28), whether it stopped before the first byte or in
//! the middle. The same script runs in a guest (through the workspace's proxy, as an HTTPS tool
//! would, with a `CONNECT` tunnel) and on the host against [`BulkHost`](crate::BulkHost), where the
//! unit tests show that a stalled stream does fail it.

use std::fmt::Write as _;

/// A transfer with no progress for this long is a stall (the bar the vsock path was accepted on).
pub const STALL_SECS: u32 = 5;

/// curl's exit status for "operation timed out", which is also what a stall ends in.
pub const CURL_TIMED_OUT: i32 = 28;

/// How the script's curl reaches the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// Through the proxy the environment names, in a `CONNECT` tunnel (a guest's route).
    EnvProxyTunnel,
    /// Straight to the address, whatever the environment says (the host-side self-test).
    Direct,
}

/// What one transfer reported (`RESULT <code> <bytes> <seconds> <exit>`).
#[derive(Clone, Debug, PartialEq)]
pub struct Transfer {
    /// The HTTP status, 0 when no reply came.
    pub http_code: u16,
    /// Body bytes received.
    pub bytes: u64,
    /// Seconds from start to end.
    pub seconds: f64,
    /// curl's exit status for this transfer.
    pub exit: i32,
}

/// The shell script that runs `count` parallel downloads of `url` and prints one `RESULT` line per
/// transfer. It always exits 0: the verdict is [`Report::problems`], not the shell's status.
///
/// `max_secs` is the cap on any one transfer, a backstop under the test's own deadline.
#[must_use]
pub fn script(url: &str, count: usize, route: Route, max_secs: u32) -> String {
    let route = match route {
        Route::EnvProxyTunnel => "--proxytunnel",
        Route::Direct => "--noproxy '*'",
    };
    let mut s = String::new();
    // The `url` goes through a curl config file so one process opens every transfer at once.
    let _ = writeln!(s, "cfg=$(mktemp) || exit 1");
    let _ = writeln!(
        s,
        "for i in $(seq 1 {count}); do printf 'url = \"%s\"\\noutput = \"/dev/null\"\\n' '{url}' >> \"$cfg\"; done"
    );
    let _ = writeln!(
        s,
        "curl -sS -Z --parallel-immediate --parallel-max {count} {route} \
         --speed-limit 1 --speed-time {STALL_SECS} --connect-timeout 30 --max-time {max_secs} \
         -w 'RESULT %{{http_code}} %{{size_download}} %{{time_total}} %{{exitcode}}\\n' -K \"$cfg\""
    );
    let _ = writeln!(s, "rm -f \"$cfg\"; exit 0");
    s
}

/// The transfers in curl's output: every `RESULT` line, other lines (curl's own messages) ignored.
///
/// # Errors
///
/// A `RESULT` line that does not have the four numbers.
pub fn parse_transfers(output: &str) -> Result<Vec<Transfer>, String> {
    let mut out = Vec::new();
    for line in output.lines().filter_map(|l| l.strip_prefix("RESULT ")) {
        let bad = || format!("unreadable result line: RESULT {line}");
        let f: Vec<&str> = line.split_whitespace().collect();
        let [code, bytes, secs, exit] = f[..] else {
            return Err(bad());
        };
        out.push(Transfer {
            http_code: code.parse().map_err(|_| bad())?,
            bytes: bytes.parse().map_err(|_| bad())?,
            seconds: secs.parse().map_err(|_| bad())?,
            exit: exit.parse().map_err(|_| bad())?,
        });
    }
    Ok(out)
}

/// A run's transfers against what was asked for.
#[derive(Debug)]
pub struct Report {
    expected: usize,
    bytes: u64,
    transfers: Vec<Transfer>,
}

impl Report {
    /// `expected` transfers of `bytes` bytes each were started; `transfers` is what came back.
    #[must_use]
    pub fn new(expected: usize, bytes: u64, transfers: Vec<Transfer>) -> Self {
        Self {
            expected,
            bytes,
            transfers,
        }
    }

    fn complete(&self, t: &Transfer) -> bool {
        t.exit == 0 && t.http_code == 200 && t.bytes == self.bytes
    }

    /// Transfers that ended with the right status and every byte.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.transfers.iter().filter(|t| self.complete(t)).count()
    }

    /// Transfers curl ended for lack of progress or time (exit 28).
    #[must_use]
    pub fn stalled(&self) -> usize {
        self.transfers
            .iter()
            .filter(|t| t.exit == CURL_TIMED_OUT)
            .count()
    }

    /// The longest any transfer took, seconds.
    #[must_use]
    pub fn slowest_secs(&self) -> f64 {
        self.transfers.iter().map(|t| t.seconds).fold(0.0, f64::max)
    }

    /// Body bytes of the transfers that completed.
    #[must_use]
    pub fn moved_bytes(&self) -> u64 {
        self.transfers
            .iter()
            .filter(|t| self.complete(t))
            .map(|t| t.bytes)
            .sum()
    }

    /// One line for the log: what ran and how it went; the reference run's record.
    #[must_use]
    pub fn summary(&self, wall_secs: f64) -> String {
        let mib = |b: u64| u64_to_f64(b) / 1_048_576.0;
        format!(
            "{} of {} transfers completed, {} stalled, slowest {:.1} s, {:.0} MiB in {:.1} s wall ({:.1} MiB/s)",
            self.completed(),
            self.expected,
            self.stalled(),
            self.slowest_secs(),
            mib(self.moved_bytes()),
            wall_secs,
            mib(self.moved_bytes()) / wall_secs.max(1e-9),
        )
    }

    /// What breaks the bar, one line each; empty when the run passes: the right count of
    /// transfers, each complete, none stalled.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.transfers.len() != self.expected {
            out.push(format!(
                "{} transfers reported, {} started",
                self.transfers.len(),
                self.expected
            ));
        }
        let stalled = self.stalled();
        if stalled > 0 {
            out.push(format!(
                "{stalled} transfers went {STALL_SECS} s without progress (or ran out of time)"
            ));
        }
        for t in self
            .transfers
            .iter()
            .filter(|t| !self.complete(t) && t.exit != CURL_TIMED_OUT)
            .take(5)
        {
            out.push(format!(
                "a transfer failed: http {}, {} of {} bytes, curl exit {}",
                t.http_code, t.bytes, self.bytes, t.exit
            ));
        }
        out
    }
}

#[expect(clippy::cast_precision_loss, reason = "a log figure, far below 2^52")]
fn u64_to_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests;
