// SPDX-License-Identifier: AGPL-3.0-or-later
//! The kept-alive request cost: one `curl` sends N small requests one after another on a single
//! connection, and the time each one took is judged. Test code only.
//!
//! The first request pays for the connection (and, in a guest, for the proxy's `CONNECT` tunnel), so
//! the figure is the mean of the others. curl reports `num_connects` per transfer: a request that
//! opened a second connection would not be a kept-alive one, so any connection beyond the first is a
//! problem, not a figure. The same script runs in a guest through the workspace's proxy and on the
//! host against [`BulkHost`](crate::BulkHost) (`/keepalive/<n>`).

use std::fmt::Write as _;

use crate::load::Route;

/// One request's line (`KEEPALIVE <http code> <connections opened> <seconds> <exit>`).
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    /// The HTTP status, 0 when no reply came.
    pub http_code: u16,
    /// Connections curl opened for this request: 1 for the first, 0 for a reused one.
    pub connects: u32,
    /// Seconds from start to end of this request.
    pub seconds: f64,
    /// curl's exit status for this request.
    pub exit: i32,
}

/// The shell script that sends `count` requests for `url` in one curl process, which keeps the
/// connection alive between them, and prints one `KEEPALIVE` line each. It always exits 0: the verdict
/// is [`Report::problems`].
#[must_use]
pub fn script(url: &str, count: usize, route: Route, max_secs: u32) -> String {
    let route = match route {
        Route::EnvProxyTunnel => "--proxytunnel",
        Route::Direct => "--noproxy '*'",
    };
    let mut s = String::new();
    let _ = writeln!(s, "cfg=$(mktemp) || exit 1");
    let _ = writeln!(
        s,
        "for i in $(seq 1 {count}); do printf 'url = \"%s\"\\noutput = \"/dev/null\"\\n' '{url}' >> \"$cfg\"; done"
    );
    let _ = writeln!(
        s,
        "curl -sS {route} --connect-timeout 30 --max-time {max_secs} \
         -w 'KEEPALIVE %{{http_code}} %{{num_connects}} %{{time_total}} %{{exitcode}}\\n' -K \"$cfg\""
    );
    let _ = writeln!(s, "rm -f \"$cfg\"; exit 0");
    s
}

/// The requests in curl's output: every `KEEPALIVE` line, other lines ignored.
///
/// # Errors
///
/// A `KEEPALIVE` line that does not have the four numbers.
pub fn parse_requests(output: &str) -> Result<Vec<Request>, String> {
    let mut out = Vec::new();
    for line in output.lines().filter_map(|l| l.strip_prefix("KEEPALIVE ")) {
        let bad = || format!("unreadable keepalive line: KEEPALIVE {line}");
        let f: Vec<&str> = line.split_whitespace().collect();
        let [code, connects, secs, exit] = f[..] else {
            return Err(bad());
        };
        out.push(Request {
            http_code: code.parse().map_err(|_| bad())?,
            connects: connects.parse().map_err(|_| bad())?,
            seconds: secs.parse().map_err(|_| bad())?,
            exit: exit.parse().map_err(|_| bad())?,
        });
    }
    Ok(out)
}

/// A run's requests against what was asked for.
#[derive(Debug)]
pub struct Report {
    expected: usize,
    requests: Vec<Request>,
}

impl Report {
    /// `expected` requests were started; `requests` is what came back.
    #[must_use]
    pub fn new(expected: usize, requests: Vec<Request>) -> Self {
        Self { expected, requests }
    }

    /// Milliseconds a request took, on average, not counting the first (which opened the connection).
    /// `None` with fewer than two requests.
    #[must_use]
    pub fn per_request_ms(&self) -> Option<f64> {
        let rest = self.requests.get(1..).filter(|r| !r.is_empty())?;
        let sum: f64 = rest.iter().map(|r| r.seconds).sum();
        Some(sum * 1000.0 / f64::from(u32::try_from(rest.len()).unwrap_or(u32::MAX)))
    }

    /// The slowest request after the first, milliseconds.
    #[must_use]
    pub fn slowest_ms(&self) -> f64 {
        self.requests
            .iter()
            .skip(1)
            .map(|r| r.seconds * 1000.0)
            .fold(0.0, f64::max)
    }

    /// One line for the log: what ran and what a request cost; the record the bar is tuned on.
    #[must_use]
    pub fn summary(&self) -> String {
        let total: f64 = self.requests.iter().map(|r| r.seconds).sum();
        format!(
            "{} of {} requests, {:.3} ms per kept-alive request (slowest {:.1} ms), {:.2} s in all",
            self.requests.len(),
            self.expected,
            self.per_request_ms().unwrap_or(f64::NAN),
            self.slowest_ms(),
            total,
        )
    }

    /// What makes the run not a kept-alive measurement, one line each; empty when it is: the right
    /// count, each a 200 with exit 0, and one connection for all of them.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.requests.len() != self.expected {
            out.push(format!(
                "{} requests reported, {} sent",
                self.requests.len(),
                self.expected
            ));
        }
        let failed = self
            .requests
            .iter()
            .filter(|r| r.exit != 0 || r.http_code != 200)
            .count();
        if failed > 0 {
            out.push(format!(
                "{failed} requests did not end in a 200 with exit 0"
            ));
        }
        let connects: u32 = self.requests.iter().map(|r| r.connects).sum();
        if connects != 1 {
            out.push(format!(
                "{connects} connections were opened, the point is one"
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests;
