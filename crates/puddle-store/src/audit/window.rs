// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-workspace limit on `connection` records (R-26).

/// Per-workspace limit on `connection` records (R-26): at most `limit` per second of a monotonic
/// clock, so a host clock that steps neither locks a workspace out nor restarts the limit; the
/// excess is counted and written as one summary record when the second is over, stamped with the
/// wall-clock second the window began in.
#[derive(Debug, Default)]
pub(crate) struct ConnectionWindow {
    second: u64,
    /// The wall-clock start of the window's second, in epoch ms; set by its first record.
    started: Option<u64>,
    written: u32,
    excess: u64,
    /// A summary whose audit write failed: `(second_start_ms, count)`, handed out again by the
    /// next [`ConnectionWindow::roll`].
    owed: Option<(u64, u64)>,
}

impl ConnectionWindow {
    /// Whether a record at wall time `now` (monotonic time `monotonic`) may be written, and a
    /// summary `(second_start_ms, count)` for a finished second that had excess.
    pub(crate) fn admit(
        &mut self,
        now: u64,
        monotonic: u64,
        limit: u32,
    ) -> (bool, Option<(u64, u64)>) {
        let finished = self.roll(monotonic);
        self.started.get_or_insert(now / 1000 * 1000);
        if self.written < limit {
            self.written += 1;
            (true, finished)
        } else {
            self.excess += 1;
            (false, finished)
        }
    }

    /// The summary of a finished second with excess, if `monotonic` is past it, merged with one
    /// owed from a failed write.
    pub(crate) fn roll(&mut self, monotonic: u64) -> Option<(u64, u64)> {
        let second = monotonic / 1000;
        let finished = if second <= self.second {
            None
        } else {
            let finished = self
                .started
                .filter(|_| self.excess > 0)
                .map(|started| (started, self.excess));
            *self = Self {
                second,
                started: None,
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
