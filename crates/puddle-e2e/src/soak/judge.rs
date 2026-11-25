// SPDX-License-Identifier: AGPL-3.0-or-later
//! The soak judge: reads a run's [`Series`] and returns the findings of the playbook's pass and
//! fail lines. Each rule is one [`Check`]; [`judge_only`] runs a chosen set, which is how the
//! tests prove that every fixture needs its check (the finding disappears without it).

use std::fmt;
use std::time::Duration;

use super::series::Series;

const HOUR: u64 = 3600;
const KIB: f64 = 1024.0;

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The run fails.
    Major,
    /// Worth a look; the run passes.
    Minor,
}

/// One rule of the judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// RSS or handles grew by more than 30 % between hour 1 (a later process's own hour 1) and the
    /// end of the active period.
    Leak,
    /// RSS, handles or threads kept rising over the idle night, beyond the measured noise.
    IdleGrowth,
    /// A call's p95 was over 500 ms in an hour after hour 4 after a baseline under 100 ms.
    ApiLatency,
    /// The event stream was silent for over 10 minutes while the loops ran, with no reconnect.
    SseSilence,
    /// Over 1 % failed requests to the forwarded web app in an hour, or any during the night.
    W4Failures,
    /// The database's write-ahead log was over 200 MiB.
    WalSize,
    /// The database grew out of line with the number of records.
    DbGrowth,
}

/// Every rule.
pub const ALL: [Check; 7] = [
    Check::Leak,
    Check::IdleGrowth,
    Check::ApiLatency,
    Check::SseSilence,
    Check::W4Failures,
    Check::WalSize,
    Check::DbGrowth,
];

/// The timeline of a run, in UTC epoch seconds.
#[derive(Debug, Clone, Copy)]
pub struct Schedule {
    /// When hour 0 starts.
    pub start: u64,
    /// How long the loops run; the idle night follows.
    pub active_secs: u64,
    /// Seconds of one playbook hour: 3600 for the real day, less for a compressed run. Every
    /// window the playbook names in hours ("hour 1", "after hour 4", "in any hour") is read in
    /// these; the 10 minutes of the SSE line stay real minutes (the page's silence is real time).
    pub hour: u64,
}

impl Schedule {
    /// The playbook's day: 8 hours active, then idle.
    #[must_use]
    pub const fn day(start: u64) -> Self {
        Self {
            start,
            active_secs: 8 * HOUR,
            hour: HOUR,
        }
    }

    const fn active_end(self) -> u64 {
        self.start + self.active_secs
    }

    /// Width of the windows "hour 1" and "hour 8" are read from: a quarter of an hour.
    const fn level_window(self) -> u64 {
        self.hour / 4
    }
}

/// One thing the judge found.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// MAJOR or MINOR.
    pub severity: Severity,
    /// The rule that found it.
    pub check: Check,
    /// The metric or call it is about.
    pub subject: String,
    /// The numbers, for the report.
    pub detail: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sev = match self.severity {
            Severity::Major => "MAJOR",
            Severity::Minor => "MINOR",
        };
        write!(
            f,
            "{sev} {:?} {}: {}",
            self.check, self.subject, self.detail
        )
    }
}

/// Runs every rule.
#[must_use]
pub fn judge(series: &Series, schedule: Schedule) -> Vec<Finding> {
    judge_only(series, schedule, &ALL)
}

/// Runs only the given rules.
#[must_use]
pub fn judge_only(series: &Series, schedule: Schedule, checks: &[Check]) -> Vec<Finding> {
    let mut out = Vec::new();
    for &check in checks {
        match check {
            Check::Leak => leak(series, schedule, &mut out),
            Check::IdleGrowth => idle_growth(series, schedule, &mut out),
            Check::ApiLatency => api_latency(series, schedule, &mut out),
            Check::SseSilence => sse_silence(series, schedule, &mut out),
            Check::W4Failures => w4_failures(series, schedule, &mut out),
            Check::WalSize => wal_size(series, &mut out),
            Check::DbGrowth => db_growth(series, schedule, &mut out),
        }
    }
    out
}

fn find(out: &mut Vec<Finding>, severity: Severity, check: Check, subject: &str, detail: String) {
    out.push(Finding {
        severity,
        check,
        subject: subject.to_owned(),
        detail,
    });
}

/// The per-process series a rule reads: the metrics `proc.*.<field>`.
fn proc_metrics<'a>(series: &'a Series, field: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    series.metrics("proc.", field)
}

fn values_in(points: &[(u64, f64)], from: u64, to: u64) -> Vec<f64> {
    points
        .iter()
        .filter(|&&(t, _)| t >= from && t < to)
        .map(|&(_, v)| v)
        .collect()
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    let upper = *v.get(mid)?;
    if v.len() % 2 == 1 {
        Some(upper)
    } else {
        Some(f64::midpoint(*v.get(mid - 1)?, upper))
    }
}

/// Nearest-rank p95.
fn p95(mut v: Vec<f64>) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    let rank = (v.len() * 95).div_ceil(100).max(1);
    v.get(rank - 1).copied()
}

fn mean(v: &[f64]) -> Option<f64> {
    #[expect(clippy::cast_precision_loss, reason = "sample counts are tiny")]
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// Samples needed in a comparison window before a level counts as measured.
const MIN_WINDOW_SAMPLES: usize = 3;
/// The level in `from..from + window`: the median of the samples there.
fn level(points: &[(u64, f64)], from: u64, window: u64) -> Option<f64> {
    let v = values_in(points, from, from + window);
    (v.len() >= MIN_WINDOW_SAMPLES).then(|| median(v)).flatten()
}

fn leak(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    for field in [".rss_kb", ".handles"] {
        for metric in proc_metrics(series, field) {
            let pts = series.points(metric);
            let w = sched.level_window();
            // A process started later in the day (Puddle restarted) is read from its own hour 1.
            let born = pts.first().map_or(sched.start, |p| p.0).max(sched.start);
            let late_from = sched.active_end().saturating_sub(w);
            let early_from = born + sched.hour;
            if early_from + w > late_from {
                continue;
            }
            let early = level(pts, early_from, w);
            let late = level(pts, late_from, w);
            let (Some(early), Some(late)) = (early, late) else {
                continue;
            };
            if early > 0.0 && late > early * 1.30 {
                find(
                    out,
                    Severity::Major,
                    Check::Leak,
                    metric,
                    format!(
                        "{early} at hour 1, {late} at hour 8: up {:.1} %, the line is 30 %",
                        (late / early - 1.0) * 100.0
                    ),
                );
            }
        }
    }
}

/// Least-squares rise over the span and the standard deviation of what the line leaves over.
/// Both are NaN for points that all share one time, which no comparison against them passes.
fn trend(points: &[(u64, f64)]) -> (f64, f64) {
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample counts and seconds are small"
    )]
    {
        let n = points.len() as f64;
        let t0 = points.first().map_or(0, |p| p.0);
        let last = points.last().map_or(0, |p| p.0);
        let xs: Vec<f64> = points.iter().map(|&(t, _)| (t - t0) as f64).collect();
        let mx = xs.iter().sum::<f64>() / n;
        let my = points.iter().map(|&(_, v)| v).sum::<f64>() / n;
        let sxx: f64 = xs.iter().map(|x| (x - mx).powi(2)).sum();
        let sxy: f64 = xs
            .iter()
            .zip(points)
            .map(|(x, &(_, y))| (x - mx) * (y - my))
            .sum();
        let slope = sxy / sxx;
        let ss: f64 = xs
            .iter()
            .zip(points)
            .map(|(x, &(_, y))| (y - (my + slope * (x - mx))).powi(2))
            .sum();
        (slope * (last - t0) as f64, (ss / n).sqrt())
    }
}

/// The smallest rise of each field worth calling growth, whatever the noise.
fn idle_floor(field: &str) -> f64 {
    match field {
        ".rss_kb" => KIB,
        _ => 1.0,
    }
}

/// Idle samples a series needs before a trend counts.
const MIN_IDLE_SAMPLES: usize = 8;

fn idle_growth(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    for field in [".rss_kb", ".handles", ".threads"] {
        for metric in proc_metrics(series, field) {
            let idle: Vec<(u64, f64)> = series
                .points(metric)
                .iter()
                .copied()
                .filter(|&(t, _)| t >= sched.active_end())
                .collect();
            if idle.len() < MIN_IDLE_SAMPLES {
                continue;
            }
            let (rise, noise) = trend(&idle);
            if rise >= idle_floor(field) && rise > 3.0 * noise {
                find(
                    out,
                    Severity::Major,
                    Check::IdleGrowth,
                    metric,
                    format!(
                        "rose {rise:.0} over the idle night (noise {noise:.1}); idle must not grow"
                    ),
                );
            }
        }
    }
}

fn api_latency(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    for metric in series.metrics("api.", ".ms") {
        let pts = series.points(metric);
        let Some(base) = p95(values_in(pts, sched.start, sched.start + sched.hour)) else {
            continue;
        };
        if base >= 100.0 {
            continue;
        }
        let last = pts.last().map_or(0, |&(t, _)| t);
        let mut from = sched.start + 4 * sched.hour;
        while from <= last {
            if let Some(p) = p95(values_in(pts, from, from + sched.hour))
                && p > 500.0
            {
                find(
                    out,
                    Severity::Major,
                    Check::ApiLatency,
                    metric,
                    format!(
                        "p95 {p} ms in hour {} against {base} ms at hour 0 (lines: 500 and 100)",
                        (from - sched.start) / sched.hour
                    ),
                );
                break;
            }
            from += sched.hour;
        }
    }
}

fn sse_silence(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    let reconnects = series.points("sse.reconnects");
    let at_or_before = |t: u64| {
        reconnects
            .iter()
            .rev()
            .find(|&&(rt, _)| rt <= t)
            .or_else(|| reconnects.first())
            .map_or(0.0, |&(_, v)| v)
    };
    let mut worst: Option<(u64, f64)> = None;
    for &(t, age) in series.points("sse.age_s") {
        if t >= sched.active_end() || age <= 600.0 {
            continue;
        }
        let silent = Duration::try_from_secs_f64(age).map_or(u64::MAX, |d| d.as_secs() + 1);
        let begun = t.saturating_sub(silent);
        let (then, now) = (at_or_before(begun), at_or_before(t));
        if now <= then && worst.is_none_or(|(_, a)| age > a) {
            worst = Some((t, age));
        }
    }
    if let Some((t, age)) = worst {
        find(
            out,
            Severity::Major,
            Check::SseSilence,
            "sse.age_s",
            format!(
                "no event for {age} s at {}:{:02} into the run and no reconnect; the page would stop updating silently",
                t.saturating_sub(sched.start) / sched.hour,
                t.saturating_sub(sched.start) % sched.hour * 60 / sched.hour
            ),
        );
    }
}

fn w4_failures(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    let pts = series.points("w4.fail_share");
    let end = sched.active_end();
    let mut from = sched.start;
    while from < end {
        let v = values_in(pts, from, (from + sched.hour).min(end));
        if let Some(m) = mean(&v)
            && m > 0.01
        {
            find(
                out,
                Severity::Major,
                Check::W4Failures,
                "w4.fail_share",
                format!(
                    "{:.1} % failed in hour {} (line: 1 %)",
                    m * 100.0,
                    (from - sched.start) / sched.hour
                ),
            );
        }
        from += sched.hour;
    }
    let night: Vec<f64> = values_in(pts, end, u64::MAX);
    if let Some(&worst) = night.iter().max_by(|a, b| a.total_cmp(b))
        && worst > 0.0
    {
        find(
            out,
            Severity::Major,
            Check::W4Failures,
            "w4.fail_share",
            format!(
                "{:.1} % failed during the night (line: none)",
                worst * 100.0
            ),
        );
    }
}

fn wal_size(series: &Series, out: &mut Vec<Finding>) {
    let limit = 200.0 * KIB * KIB;
    if let Some(&(_, max)) = series
        .points("file.db_wal.bytes")
        .iter()
        .max_by(|a, b| a.1.total_cmp(&b.1))
        && max > limit
    {
        find(
            out,
            Severity::Minor,
            Check::WalSize,
            "file.db_wal.bytes",
            format!("{max} bytes, the line is 200 MiB"),
        );
    }
}

/// Bytes the database may add per record added over the run.
const DB_BYTES_PER_RECORD: f64 = 8.0 * KIB;

fn db_growth(series: &Series, sched: Schedule, out: &mut Vec<Finding>) {
    let w = sched.level_window();
    let from = sched.start + sched.hour;
    let to = sched.active_end().saturating_sub(w);
    let (Some(db0), Some(db1)) = (
        level(series.points("file.db.bytes"), from, w),
        level(series.points("file.db.bytes"), to, w),
    ) else {
        return;
    };
    let (Some(n0), Some(n1)) = (
        level(series.points("count.audit"), from, w),
        level(series.points("count.audit"), to, w),
    ) else {
        return;
    };
    let grown = db1 - db0;
    let added = n1 - n0;
    let out_of_line = if added > 0.0 {
        grown / added > DB_BYTES_PER_RECORD
    } else {
        db0 > 0.0 && grown / db0 > 0.10
    };
    if out_of_line {
        find(
            out,
            Severity::Minor,
            Check::DbGrowth,
            "file.db.bytes",
            format!(
                "grew {grown} bytes for {added} records; the line is {DB_BYTES_PER_RECORD} per record, or 10 % with no new records"
            ),
        );
    }
}

#[cfg(test)]
mod tests;
