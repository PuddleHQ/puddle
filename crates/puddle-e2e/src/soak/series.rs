// SPDX-License-Identifier: AGPL-3.0-or-later
//! `metrics.tsv`: one line per measurement, `time<TAB>metric<TAB>value`, time in UTC epoch seconds.
//! A sample is the lines that share a time. Lines starting with `#` are comments.
//!
//! Metric names (the sampler writes them, the judge reads them):
//!
//! | Name | Meaning |
//! |---|---|
//! | `host.free_ram_kb`, `host.cpu_pct`, `host.free_disk_kb`, `host.commit_pct` | the host |
//! | `proc.<label>.<pid>.rss_kb`, `.threads`, `.handles` | one process (handles are file descriptors on Linux) |
//! | `file.<name>.bytes` | a data file; `db`, `db_wal`, plus one per volume and root disk |
//! | `api.<call>.ms` | latency of one call: `health`, `workspaces`, `pending`, `audit` |
//! | `sse.age_s`, `sse.reconnects` | seconds since the last event, reconnects so far |
//! | `count.pending`, `count.audit`, `count.notices` | records now |
//! | `w4.fail_share` | share of the last 5 minutes' 10-second requests that failed (0 to 1) |

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// One line of `metrics.tsv`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// UTC epoch seconds.
    pub time: u64,
    /// The metric name.
    pub metric: String,
    /// Its value.
    pub value: f64,
}

/// A line that is not `time<TAB>metric<TAB>value`.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    /// What is wrong with it.
    pub why: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "metrics line {}: {}", self.line, self.why)
    }
}

impl std::error::Error for ParseError {}

/// All rows of a run, grouped by metric and sorted by time.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Series {
    by_metric: BTreeMap<String, Vec<(u64, f64)>>,
}

impl Series {
    /// Reads the text of a `metrics.tsv`.
    ///
    /// # Errors
    /// The first line that does not have three tab-separated fields with a whole time and a
    /// finite number.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut series = Self::default();
        for (i, line) in text.lines().enumerate() {
            let line_no = i + 1;
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split('\t');
            let (Some(time), Some(metric), Some(value), None) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                return Err(ParseError {
                    line: line_no,
                    why: "expected three tab-separated fields",
                });
            };
            let time = time.parse::<u64>().map_err(|_| ParseError {
                line: line_no,
                why: "the time is not whole epoch seconds",
            })?;
            let value = value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite())
                .ok_or(ParseError {
                    line: line_no,
                    why: "the value is not a finite number",
                })?;
            series.push(time, metric, value);
        }
        Ok(series)
    }

    /// Adds one measurement.
    pub fn push(&mut self, time: u64, metric: &str, value: f64) {
        let points = self.by_metric.entry(metric.to_owned()).or_default();
        let at = points.partition_point(|&(t, _)| t <= time);
        points.insert(at, (time, value));
    }

    /// The text of the file: every measurement, oldest first within its metric.
    #[must_use]
    pub fn render(&self) -> String {
        let mut rows: Vec<(u64, &str, f64)> = self
            .by_metric
            .iter()
            .flat_map(|(m, pts)| pts.iter().map(move |&(t, v)| (t, m.as_str(), v)))
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(b.1)));
        let mut out = String::from("# time_utc_s\tmetric\tvalue\n");
        for (t, m, v) in rows {
            let _ = writeln!(out, "{t}\t{m}\t{v}");
        }
        out
    }

    /// The points of one metric, oldest first; empty when it was never measured.
    #[must_use]
    pub fn points(&self, metric: &str) -> &[(u64, f64)] {
        self.by_metric.get(metric).map_or(&[], Vec::as_slice)
    }

    /// Every metric name that starts with `prefix` and ends with `suffix`.
    pub fn metrics<'a>(
        &'a self,
        prefix: &'a str,
        suffix: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        self.by_metric
            .keys()
            .map(String::as_str)
            .filter(move |m| m.starts_with(prefix) && m.ends_with(suffix))
    }

    /// The earliest time of any metric.
    #[must_use]
    pub fn first_time(&self) -> Option<u64> {
        self.by_metric
            .values()
            .filter_map(|p| p.first().map(|&(t, _)| t))
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_series_survives_a_round_trip_through_its_file_text() {
        let mut s = Series::default();
        s.push(300, "sse.age_s", 4.0);
        s.push(0, "sse.age_s", 1.5);
        s.push(0, "count.pending", 2.0);
        let back = Series::parse(&s.render()).unwrap();
        assert_eq!(back, s);
        assert_eq!(back.points("sse.age_s"), &[(0, 1.5), (300, 4.0)]);
        assert_eq!(back.first_time(), Some(0));
        assert_eq!(back.points("nope"), []);
    }

    #[test]
    fn comments_and_blank_lines_are_skipped_and_bad_lines_name_their_number() {
        let ok = Series::parse("# c\n\n5\ta.b\t1\n").unwrap();
        assert_eq!(ok.points("a.b"), &[(5, 1.0)]);
        let bad = |text: &str| Series::parse(text).unwrap_err();
        assert_eq!(bad("5\ta\n").line, 1);
        assert_eq!(bad("1\ta\t1\n2\ta\t1\t9\n").line, 2);
        assert_eq!(bad("x\ta\t1\n").why, "the time is not whole epoch seconds");
        assert_eq!(bad("1\ta\tNaN\n").why, "the value is not a finite number");
        assert_eq!(
            bad("1\ta\tx\n").to_string(),
            "metrics line 1: the value is not a finite number"
        );
    }

    #[test]
    fn metrics_are_listed_by_prefix_and_suffix() {
        let s = Series::parse("1\tproc.a.1.rss_kb\t1\n1\tproc.a.1.handles\t1\n1\tapi.x.ms\t1\n")
            .unwrap();
        let got: Vec<_> = s.metrics("proc.", ".rss_kb").collect();
        assert_eq!(got, ["proc.a.1.rss_kb"]);
    }
}
