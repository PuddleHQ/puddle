// SPDX-License-Identifier: AGPL-3.0-or-later
//! Synthetic runs: 12 hours, a sample every 5 minutes, 8 hours of load and then the idle night.

use super::*;

const START: u64 = 1_000_000;
const STEP: u64 = 300;
const ACTIVE_END_H: f64 = 8.0;

fn sched() -> Schedule {
    Schedule::day(START)
}

/// A little deterministic jitter in `0..=span`, the same on every run.
fn jitter(i: u64, span: u64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "small test numbers")]
    {
        ((i * 7919) % (span + 1)) as f64
    }
}

/// Builds a run; `extra` adds or replaces nothing: it only pushes the metrics a test shapes, on
/// top of a flat, quiet baseline of every metric the judge reads.
fn run(extra: impl Fn(&mut Series, u64, f64)) -> Series {
    let mut s = Series::default();
    for i in 0..=(12 * 3600 / STEP) {
        let t = START + i * STEP;
        #[expect(clippy::cast_precision_loss, reason = "small test numbers")]
        let h = (i * STEP) as f64 / 3600.0;
        s.push(t, "proc.worker.100.threads", 40.0);
        s.push(t, "file.db_wal.bytes", 1.0e6);
        s.push(t, "sse.age_s", jitter(i, 30));
        s.push(t, "sse.reconnects", 0.0);
        s.push(t, "w4.fail_share", 0.0);
        s.push(t, "api.health.ms", 60.0 + jitter(i, 20));
        s.push(t, "api.audit.ms", 80.0);
        s.push(t, "file.db.bytes", 5.0e6);
        s.push(t, "count.audit", 1000.0);
        extra(&mut s, t, h);
    }
    s
}

/// A process whose RSS is `rss(hours)` and whose handles are flat.
fn with_rss(rss: impl Fn(f64) -> f64) -> Series {
    run(move |s, t, h| {
        s.push(t, "proc.worker.100.rss_kb", rss(h));
        s.push(t, "proc.worker.100.handles", 200.0);
    })
}

fn flat_rss(h: f64) -> f64 {
    let _ = h;
    100_000.0
}

/// Every rule but the coverage one, for a test that shapes one metric of a partial series.
fn rules_only(series: &Series, schedule: Schedule) -> Vec<Finding> {
    judge_only(series, schedule, &checks_without(Check::Coverage))
}

fn checks_without(check: Check) -> Vec<Check> {
    ALL.iter().copied().filter(|&c| c != check).collect()
}

/// Asserts `series` has a finding of `check`, that the judge with every other rule still has none
/// of it (the rule is the only thing that finds it), and returns the finding.
fn found_only_by(series: &Series, check: Check, severity: Severity) -> Finding {
    let all = judge(series, sched());
    let finding = all
        .iter()
        .find(|f| f.check == check)
        .unwrap_or_else(|| panic!("{check:?} found nothing in {all:?}"));
    assert_eq!(finding.severity, severity, "{finding}");
    assert!(
        judge_only(series, sched(), &checks_without(check))
            .iter()
            .all(|f| f.check != check),
        "{check:?} was still reported without its rule"
    );
    finding.clone()
}

#[test]
fn flat_series_with_noise_get_no_finding() {
    let s = run(|s, t, h| {
        let i = (t - START) / STEP;
        s.push(t, "proc.worker.100.rss_kb", 100_000.0 + jitter(i, 400));
        s.push(t, "proc.worker.100.handles", 200.0 + jitter(i, 1));
        let _ = h;
    });
    assert_eq!(rules_only(&s, sched()), []);
}

#[test]
fn a_31_percent_rss_rise_from_hour_1_to_hour_8_is_major() {
    let s = with_rss(|h| if h < 2.0 { 100_000.0 } else { 131_000.0 });
    let f = found_only_by(&s, Check::Leak, Severity::Major);
    assert_eq!(f.subject, "proc.worker.100.rss_kb");
    assert!(f.detail.contains("31.0 %"), "{}", f.detail);
}

#[test]
fn a_29_percent_rise_is_quiet() {
    let s = with_rss(|h| if h < 2.0 { 100_000.0 } else { 129_000.0 });
    assert_eq!(judge(&s, sched()), []);
}

#[test]
fn a_handle_count_up_31_percent_is_major() {
    let s = run(|s, t, h| {
        s.push(t, "proc.vm-qa-soak-build.7.rss_kb", 100_000.0);
        s.push(
            t,
            "proc.vm-qa-soak-build.7.handles",
            if h < 2.0 { 200.0 } else { 262.0 },
        );
    });
    let f = found_only_by(&s, Check::Leak, Severity::Major);
    assert_eq!(f.subject, "proc.vm-qa-soak-build.7.handles");
}

#[test]
fn a_rise_that_stays_in_the_load_before_hour_1_is_not_a_leak() {
    // Warm-up from 50 MB to 100 MB in the first hour; hour 1 to hour 8 is flat.
    let s = with_rss(|h| {
        if h < 1.0 {
            50_000.0 + 50_000.0 * h
        } else {
            100_000.0
        }
    });
    assert_eq!(judge(&s, sched()), []);
}

#[test]
fn any_idle_slope_above_noise_is_major() {
    // 1 MiB an hour over the four idle hours; flat under load.
    let s = with_rss(|h| {
        if h < ACTIVE_END_H {
            100_000.0
        } else {
            100_000.0 + 1024.0 * (h - ACTIVE_END_H)
        }
    });
    let f = found_only_by(&s, Check::IdleGrowth, Severity::Major);
    assert_eq!(f.subject, "proc.worker.100.rss_kb");
    assert!(
        judge(&s, sched()).iter().all(|f| f.check != Check::Leak),
        "the rise is only at night, so the hour 1 to 8 line must stay quiet"
    );
}

#[test]
fn one_more_handle_during_the_idle_night_is_major() {
    let s = run(|s, t, h| {
        s.push(t, "proc.worker.100.rss_kb", 100_000.0);
        s.push(
            t,
            "proc.worker.100.handles",
            if h < 10.0 { 200.0 } else { 201.0 },
        );
    });
    let f = found_only_by(&s, Check::IdleGrowth, Severity::Major);
    assert_eq!(f.subject, "proc.worker.100.handles");
}

#[test]
fn idle_noise_without_a_trend_is_quiet() {
    let s = run(|s, t, h| {
        let i = (t - START) / STEP;
        let night = if h < ACTIVE_END_H {
            0.0
        } else {
            jitter(i, 2000)
        };
        s.push(t, "proc.worker.100.rss_kb", 100_000.0 + night);
    });
    assert_eq!(judge(&s, sched()), []);
}

#[test]
fn the_morning_round_after_the_night_is_not_idle_growth() {
    // Flat through the night, then the morning round loads the program: a step after hour 12.
    let s = with_rss(|h| if h <= 12.0 { 100_000.0 } else { 118_000.0 });
    let late = Schedule {
        idle_secs: 4 * HOUR - 1800,
        ..sched()
    };
    assert!(
        judge(&s, late).iter().all(|f| f.check != Check::IdleGrowth),
        "{:?}",
        judge(&s, late)
    );
}

#[test]
fn a_600_ms_p95_after_hour_4_against_80_ms_at_hour_0_is_major() {
    let s = run(|s, t, h| {
        s.push(t, "api.workspaces.ms", if h < 4.0 { 80.0 } else { 600.0 });
    });
    let f = found_only_by(&s, Check::ApiLatency, Severity::Major);
    assert_eq!(f.subject, "api.workspaces.ms");
    assert!(f.detail.contains("hour 4"), "{}", f.detail);
}

#[test]
fn latency_that_is_slow_from_the_start_or_only_before_hour_4_is_quiet() {
    let from_start = run(|s, t, _| s.push(t, "api.pending.ms", 600.0));
    assert_eq!(judge(&from_start, sched()), []);
    let early_only = run(|s, t, h| {
        s.push(
            t,
            "api.pending.ms",
            if (1.0..3.0).contains(&h) { 900.0 } else { 80.0 },
        );
    });
    assert_eq!(judge(&early_only, sched()), []);
    let just_ok = run(|s, t, h| s.push(t, "api.pending.ms", if h < 4.0 { 80.0 } else { 500.0 }));
    assert_eq!(
        judge(&just_ok, sched()),
        [],
        "500 ms is the line, not over it"
    );
}

/// `base` with `metric` replaced at the samples where `f(sample index, hours)` gives a value.
fn shape(base: &Series, metric: &str, f: impl Fn(u64, f64) -> Option<f64>) -> Series {
    let mut out = Series::default();
    for line in base.render().lines().filter(|l| !l.starts_with('#')) {
        let mut fields = line.split('\t');
        let (time, name, value) = (
            fields.next().unwrap(),
            fields.next().unwrap(),
            fields.next().unwrap(),
        );
        let (time, value): (u64, f64) = (time.parse().unwrap(), value.parse().unwrap());
        let index = (time - START) / STEP;
        #[expect(clippy::cast_precision_loss, reason = "small test numbers")]
        let hours = (time - START) as f64 / 3600.0;
        let value = if name == metric {
            f(index, hours).unwrap_or(value)
        } else {
            value
        };
        out.push(time, name, value);
    }
    out
}

/// A run whose event stream was silent `age` seconds at sample `at_sample`, with `reconnects(i)`.
fn sse_run(age: f64, at_sample: u64, reconnects: impl Fn(u64) -> f64) -> Series {
    let base = run(|_, _, _| {});
    let aged = shape(&base, "sse.age_s", |i, _| (i == at_sample).then_some(age));
    shape(&aged, "sse.reconnects", |i, _| Some(reconnects(i)))
}

#[test]
fn a_601_second_sse_gap_with_no_reconnect_is_major() {
    let s = sse_run(601.0, 36, |_| 0.0);
    let f = found_only_by(&s, Check::SseSilence, Severity::Major);
    assert!(f.detail.contains("601"), "{}", f.detail);
}

#[test]
fn a_600_second_gap_or_a_gap_with_a_reconnect_is_quiet() {
    assert_eq!(judge(&sse_run(600.0, 36, |_| 0.0), sched()), []);
    // The reconnect came at the sample before the long age was read.
    let with_reconnect = sse_run(601.0, 36, |i| if i >= 35 { 1.0 } else { 0.0 });
    assert_eq!(judge(&with_reconnect, sched()), []);
}

#[test]
fn silence_after_the_loops_stopped_is_not_judged() {
    // Sample 120 is hour 10, during the night.
    assert_eq!(judge(&sse_run(900.0, 120, |_| 0.0), sched()), []);
}

#[test]
fn two_percent_w4_failures_in_an_hour_are_major_and_one_percent_is_quiet() {
    let base = run(|_, _, _| {});
    let hour_3 = |v: f64| move |_: u64, h: f64| (3.0..4.0).contains(&h).then_some(v);
    let two = shape(&base, "w4.fail_share", hour_3(0.02));
    let f = found_only_by(&two, Check::W4Failures, Severity::Major);
    assert!(f.detail.contains("hour 3"), "{}", f.detail);
    let one = shape(&base, "w4.fail_share", hour_3(0.01));
    assert_eq!(judge(&one, sched()), []);
}

#[test]
fn one_failed_request_share_in_the_night_is_major() {
    let base = run(|_, _, _| {});
    let s = shape(&base, "w4.fail_share", |_, h| {
        (h > 9.0 && h < 9.1).then_some(0.003)
    });
    let f = found_only_by(&s, Check::W4Failures, Severity::Major);
    assert!(f.detail.contains("night"), "{}", f.detail);
}

#[test]
fn a_wal_over_200_mib_is_minor() {
    let base = run(|_, _, _| {});
    let s = shape(&base, "file.db_wal.bytes", |_, h| {
        Some(if h > 5.0 {
            210.0 * 1024.0 * 1024.0
        } else {
            1.0e6
        })
    });
    found_only_by(&s, Check::WalSize, Severity::Minor);
    let ok = shape(&base, "file.db_wal.bytes", |_, _| {
        Some(199.0 * 1024.0 * 1024.0)
    });
    assert_eq!(judge(&ok, sched()), []);
}

#[test]
fn a_database_that_grows_far_more_than_its_records_is_minor() {
    let base = run(|_, _, _| {});
    // 1000 records added, 20 MB added: 20 kB a record.
    let records = shape(&base, "count.audit", |_, h| {
        Some(if h < 2.0 { 1000.0 } else { 2000.0 })
    });
    let big = shape(&records, "file.db.bytes", |_, h| {
        Some(if h < 2.0 { 5.0e6 } else { 25.0e6 })
    });
    found_only_by(&big, Check::DbGrowth, Severity::Minor);
    // The same records with 4 MB added: 4 kB a record.
    let fine = shape(&records, "file.db.bytes", |_, h| {
        Some(if h < 2.0 { 5.0e6 } else { 9.0e6 })
    });
    assert_eq!(judge(&fine, sched()), []);
    // No new records and the file up by a fifth.
    let none = shape(&base, "file.db.bytes", |_, h| {
        Some(if h < 2.0 { 5.0e6 } else { 6.0e6 })
    });
    found_only_by(&none, Check::DbGrowth, Severity::Minor);
}

#[test]
fn a_finding_prints_its_severity_rule_subject_and_numbers() {
    let s = with_rss(|h| if h < 2.0 { 100_000.0 } else { 131_000.0 });
    let text = judge(&s, sched())[0].to_string();
    assert!(
        text.starts_with("MAJOR Leak proc.worker.100.rss_kb: "),
        "{text}"
    );
    let _ = flat_rss(0.0);
}

#[test]
fn the_median_and_the_p95_of_a_few_values() {
    assert_eq!(median(vec![4.0, 1.0, 3.0, 2.0]), Some(2.5));
    assert_eq!(median(vec![3.0, 1.0, 2.0]), Some(2.0));
    assert_eq!(median(vec![]), None);
    assert_eq!(p95(vec![]), None);
    assert_eq!(p95((1..=20).map(f64::from).collect()), Some(19.0));
    assert_eq!(p95(vec![7.0]), Some(7.0));
}

#[test]
fn a_minor_finding_prints_as_minor() {
    let base = run(|_, _, _| {});
    let s = shape(&base, "file.db_wal.bytes", |_, _| {
        Some(300.0 * 1024.0 * 1024.0)
    });
    let text = judge(&s, sched())[0].to_string();
    assert!(
        text.starts_with("MINOR WalSize file.db_wal.bytes: "),
        "{text}"
    );
}

#[test]
fn too_little_data_is_not_judged() {
    // Three idle samples that rise: too few to call a trend.
    let mut s = Series::default();
    for i in 0..3_u64 {
        let t = START + 8 * 3600 + i * STEP;
        s.push(
            t,
            "proc.worker.100.rss_kb",
            100_000.0 + 50_000.0 * f64::from(u32::try_from(i).unwrap()),
        );
    }
    assert_eq!(rules_only(&s, sched()), []);
    // A latency that was never measured in hour 0 has no baseline.
    let late = run(|s, t, h| {
        if h >= 3.0 {
            s.push(t, "api.late.ms", 900.0);
        }
    });
    assert_eq!(rules_only(&late, sched()), []);
    // A database size with no record count cannot be compared with it.
    let mut db_only = Series::default();
    for i in 0..=(8 * 12_u64) {
        let big = if i < 24 { 5.0e6 } else { 50.0e6 };
        db_only.push(START + i * STEP, "file.db.bytes", big);
    }
    assert_eq!(rules_only(&db_only, sched()), []);
}

#[test]
fn a_silence_with_no_reconnect_series_at_all_counts_as_no_reconnect() {
    let mut s = Series::default();
    s.push(START + 3600, "sse.age_s", 700.0);
    let f = rules_only(&s, sched());
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].check, Check::SseSilence);
}

/// The same day squeezed into 2 minutes an hour: every time of `with_rss`'s run scaled by `hour`
/// over 3600, as a compressed soak samples it.
fn squeezed(series: &Series, hour: u64) -> Series {
    let mut out = Series::default();
    for metric in series.metrics("", "") {
        for &(t, v) in series.points(metric) {
            out.push(START + (t - START) * hour / 3600, metric, v);
        }
    }
    out
}

#[test]
fn a_compressed_run_is_judged_in_its_own_hours() {
    let hour = 120;
    let s = squeezed(
        &with_rss(|h| if h < 2.0 { 100_000.0 } else { 131_000.0 }),
        hour,
    );
    let compressed = Schedule {
        start: START,
        active_secs: 8 * hour,
        idle_secs: 4 * hour,
        hour,
    };
    let found = judge(&s, compressed);
    assert!(
        found
            .iter()
            .any(|f| f.check == Check::Leak && f.severity == Severity::Major),
        "{found:?}"
    );
    // Read in real hours the whole run sits before "hour 1", so nothing is found: the scale is
    // what makes the leak visible.
    assert_eq!(rules_only(&s, Schedule::day(START)), []);
    let flat = squeezed(&with_rss(flat_rss), hour);
    assert_eq!(judge(&flat, compressed), []);
}

#[test]
fn a_server_restarted_at_hour_5_is_judged_from_its_own_hour_1() {
    let s = run(|s, t, h| {
        if h >= 5.0 {
            let rss = if h < 7.0 { 100_000.0 } else { 131_000.0 };
            s.push(t, "proc.puddle.200.rss_kb", rss);
        }
    });
    let f = found_only_by(&s, Check::Leak, Severity::Major);
    assert_eq!(f.subject, "proc.puddle.200.rss_kb");
    let flat = run(|s, t, h| {
        if h >= 5.0 {
            s.push(t, "proc.puddle.200.rss_kb", 100_000.0);
        }
    });
    assert_eq!(judge(&flat, sched()), []);
}

#[test]
fn a_server_started_too_late_for_two_windows_is_not_judged() {
    let s = run(|s, t, h| {
        if h >= 7.0 {
            s.push(
                t,
                "proc.puddle.300.rss_kb",
                if h < 7.5 { 1.0 } else { 1000.0 },
            );
        }
    });
    assert_eq!(judge(&s, sched()), []);
}

/// The sampler of a Windows run: the Puddle processes carry the executable's name, with a dash.
fn windows_run(handles: impl Fn(u64, f64) -> f64, commit_kb: impl Fn(f64) -> f64) -> Series {
    run(|s, t, h| {
        let i = (t - START) / STEP;
        s.push(t, "proc.puddle-app.4120.rss_kb", 150_000.0);
        s.push(t, "proc.puddle-app.4120.handles", handles(i, h));
        s.push(t, "host.commit_limit_kb", 20_000_000.0);
        s.push(t, "host.commit_kb", commit_kb(h));
    })
}

#[test]
fn a_windows_app_whose_handles_creep_up_through_the_idle_hour_is_major() {
    // One more handle every sample from the end of the load on (a leaked handle per cycle).
    let s = windows_run(
        |i, h| {
            if h < ACTIVE_END_H {
                900.0
            } else {
                900.0 + f64::from(u32::try_from(i - 96).unwrap())
            }
        },
        |_| 8_000_000.0,
    );
    let f = found_only_by(&s, Check::IdleGrowth, Severity::Major);
    assert_eq!(f.subject, "proc.puddle-app.4120.handles");
}

#[test]
fn windows_handle_counts_that_move_within_noise_through_the_idle_hour_are_quiet() {
    let s = windows_run(|i, _| 900.0 + jitter(i, 3), |_| 8_000_000.0);
    assert_eq!(judge(&s, sched()), []);
}

#[test]
fn a_commit_charge_within_5_percent_of_the_limit_is_major() {
    // 19.2 GB of 20 GB: 96 %, from hour 6 on.
    let s = windows_run(
        |_, _| 900.0,
        |h| if h < 6.0 { 8_000_000.0 } else { 19_200_000.0 },
    );
    let f = found_only_by(&s, Check::CommitCharge, Severity::Major);
    assert_eq!(f.subject, "host.commit_kb");
}

#[test]
fn a_commit_charge_at_94_percent_of_the_limit_is_quiet() {
    let s = windows_run(|_, _| 900.0, |_| 18_800_000.0);
    assert_eq!(judge(&s, sched()), []);
}

#[test]
fn a_run_with_no_commit_readings_has_no_commit_finding() {
    // The Linux sampler writes no commit lines: the rule has nothing to read and says nothing.
    assert_eq!(judge(&with_rss(flat_rss), sched()), []);
}

fn coverage_only(series: &Series) -> Vec<Finding> {
    judge_only(series, sched(), &[Check::Coverage])
}

#[test]
fn a_complete_run_has_full_coverage() {
    assert_eq!(coverage_only(&with_rss(flat_rss)), []);
}

#[test]
fn a_run_with_no_samples_has_no_coverage() {
    let f = coverage_only(&Series::default());
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].severity, Severity::Major);
}

#[test]
fn a_run_with_no_process_series_is_major() {
    let s = shape(&run(|_, _, _| {}), "proc.worker.100.threads", |_, _| None);
    let mut without = Series::default();
    for m in s.metrics("", "").filter(|m| !m.starts_with("proc.")) {
        for &(t, v) in s.points(m) {
            without.push(t, m, v);
        }
    }
    let f = coverage_only(&without);
    assert!(
        f.iter()
            .any(|f| f.severity == Severity::Major && f.detail.contains("process")),
        "{f:?}"
    );
}

#[test]
fn samples_that_stop_before_the_night_ends_are_major() {
    // A sampler that died at hour 6 leaves a leak-free, idle-quiet series that every rule passes.
    let mut s = Series::default();
    for i in 0..=(6 * 3600 / STEP) {
        s.push(START + i * STEP, "proc.worker.100.rss_kb", 100_000.0);
    }
    assert_eq!(
        judge_only(&s, sched(), &checks_without(Check::Coverage)),
        []
    );
    let f = found_only_by(&s, Check::Coverage, Severity::Major);
    assert!(f.detail.contains("stopped"), "{f}");
}

#[test]
fn samples_that_start_late_are_major() {
    let mut s = Series::default();
    for i in (3 * 3600 / STEP)..=(12 * 3600 / STEP) {
        s.push(START + i * STEP, "proc.worker.100.rss_kb", 100_000.0);
    }
    let f = found_only_by(&s, Check::Coverage, Severity::Major);
    assert!(f.detail.contains("first sample"), "{f}");
}

#[test]
fn far_fewer_samples_than_the_pace_gives_are_major() {
    // Samples at both ends and one an hour between: present, but a tenth of the pace.
    let mut s = Series::default();
    for i in 0..=12_u64 {
        s.push(START + i * 3600, "proc.worker.100.rss_kb", 100_000.0);
    }
    let f = found_only_by(&s, Check::Coverage, Severity::Major);
    assert!(f.detail.contains("sample times"), "{f}");
}
