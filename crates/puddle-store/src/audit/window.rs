// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-workspace limit on `connection` records (R-26).

/// Per-workspace limit on `connection` records (R-26): at most `limit` per wall-clock second; the
/// excess is counted and written as one summary record when the second is over.
#[derive(Debug, Default)]
pub(crate) struct ConnectionWindow {
    second: u64,
    written: u32,
    excess: u64,
    /// A summary whose audit write failed: `(second_start_ms, count)`, handed out again by the
    /// next [`ConnectionWindow::roll`].
    owed: Option<(u64, u64)>,
}

impl ConnectionWindow {
    /// Whether a record at `now` may be written, and a summary `(second_start_ms, count)` for a
    /// finished second that had excess.
    pub(crate) fn admit(&mut self, now: u64, limit: u32) -> (bool, Option<(u64, u64)>) {
        let finished = self.roll(now);
        if self.written < limit {
            self.written += 1;
            (true, finished)
        } else {
            self.excess += 1;
            (false, finished)
        }
    }

    /// The summary of a finished second with excess, if `now` is past it, merged with one owed
    /// from a failed write.
    pub(crate) fn roll(&mut self, now: u64) -> Option<(u64, u64)> {
        let second = now / 1000;
        // A clock that stepped back stays in the current window, so the limit does not restart.
        let finished = if second <= self.second {
            None
        } else {
            let finished = (self.excess > 0).then_some((self.second * 1000, self.excess));
            *self = Self {
                second,
                written: 0,
                excess: 0,
                owed: self.owed,
            };
            finished
        };
        match (self.owed.take(), finished) {
            (Some((a_ts, a)), Some((b_ts, b))) => Some((a_ts.min(b_ts), a + b)),
            (owed, finished) => owed.or(finished),
        }
    }

    /// Keeps a summary whose audit write failed, to hand out again.
    pub(crate) fn owe(&mut self, ts: u64, count: u64) {
        self.owed = match self.owed {
            Some((owed_ts, owed)) => Some((owed_ts.min(ts), owed + count)),
            None => Some((ts, count)),
        };
    }
}
