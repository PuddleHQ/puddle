// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn steady(n: i64) -> Vec<i64> {
    (1_000..1_000 + n).collect()
}

#[test]
fn a_steady_heartbeat_passes() {
    let gaps = judge_heartbeat(&steady(600), &steady(600), 500).unwrap();
    assert_eq!(gaps.count, 0);
}

#[test]
fn a_route_paused_for_ten_seconds_fails_the_heartbeat() {
    let mut ticks = steady(300);
    ticks.extend((310..610).map(|t| t + 1_000));
    let err = judge_heartbeat(&ticks, &steady(600), 500).unwrap_err();
    assert!(err.contains("11 s"), "{err}");
}

#[test]
fn a_gap_of_exactly_the_bar_passes_and_one_second_more_fails() {
    let mut ticks = steady(100);
    ticks.push(1_099 + HEARTBEAT_MAX_GAP_SECS);
    assert!(judge_heartbeat(&ticks, &steady(100), 50).is_ok());
    let mut ticks = steady(100);
    ticks.push(1_099 + HEARTBEAT_MAX_GAP_SECS + 1);
    assert!(judge_heartbeat(&ticks, &steady(100), 50).is_err());
}

#[test]
fn a_gap_the_guests_own_probe_shares_is_the_runners_stall_and_passes() {
    let mut ticks = steady(300);
    ticks.extend(1_308..1_600);
    let mut probe = steady(300);
    probe.extend(1_308..1_600);
    let gaps = judge_heartbeat(&ticks, &probe, 100).unwrap();
    assert_eq!(gaps.longest, 9);
}

#[test]
fn a_gap_beside_a_probe_that_kept_ticking_is_the_routes_and_fails() {
    let mut ticks = steady(300);
    ticks.extend(1_310..1_600);
    let err = judge_heartbeat(&ticks, &steady(600), 100).unwrap_err();
    assert!(err.contains("11 s"), "{err}");
}

#[test]
fn a_probe_stall_elsewhere_excuses_nothing() {
    let mut ticks = steady(300);
    ticks.extend(1_310..1_600);
    let mut probe = steady(100);
    probe.extend(1_110..1_600);
    assert!(judge_heartbeat(&ticks, &probe, 100).is_err());
}

#[test]
fn a_heartbeat_that_never_ran_is_not_a_clean_one() {
    assert!(judge_heartbeat(&[], &[], 1).is_err());
    assert!(judge_heartbeat(&steady(10), &steady(10), 500).is_err());
}

#[test]
fn a_tree_that_came_back_equal_has_no_changes_and_new_files_are_not_changes() {
    let before = parse_tree("obj/a.gz 1700000000.123456789 100\nobj/b.gz 1700000001.5 200\n");
    let after = parse_tree(
        "obj/a.gz 1700000000.123456789 100\nobj/b.gz 1700000001.5 200\nobj/c.gz 1700000009.0 5\n",
    );
    assert_eq!(before.len(), 2);
    assert_eq!(tree_changes(&before, &after), Vec::<String>::new());
}

#[test]
fn a_lost_file_a_reset_mtime_and_a_cut_file_are_each_a_change() {
    let before = parse_tree("a 10.5 100\nb 20.5 200\nc 30.5 300\n");
    let after = parse_tree("a 10.5 100\nb 99.0 200\nc 30.5 150\n");
    let changes = tree_changes(&before, &after);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert!(changes.iter().any(|c| c.starts_with("b:")));
    assert!(changes.iter().any(|c| c.starts_with("c:")));
    let lost = tree_changes(&before, &parse_tree("a 10.5 100\n"));
    assert_eq!(lost, vec!["b: gone".to_owned(), "c: gone".to_owned()]);
}

#[test]
fn an_empty_listing_before_is_not_a_pass_the_caller_checks_the_count() {
    assert!(parse_tree("").is_empty());
    assert!(parse_tree("only-a-path\nx y z\n").is_empty());
}

#[test]
fn an_incremental_rebuild_is_short_and_a_full_one_is_not() {
    assert!(judge_incremental(1, 2, 3.0, 300.0).is_ok());
    assert!(judge_incremental(600, 2, 280.0, 300.0).is_err());
    assert!(judge_incremental(1, 2, 120.0, 300.0).is_err());
    // A short full build never makes the bar under 10 s.
    assert!(judge_incremental(0, 2, 9.0, 20.0).is_ok());
}

fn sample(at_ms: i64, took_ms: f64) -> Sample {
    Sample { at_ms, took_ms }
}

/// A witness that ticked every 100 ms from 0 to `to_ms`, minus the ticks in `gap`.
fn witness(to_ms: i64, gap: std::ops::Range<i64>) -> Vec<i64> {
    (0..=to_ms / 100)
        .map(|n| n * 100)
        .filter(|t| !gap.contains(t))
        .collect()
}

#[test]
fn a_slow_answer_or_too_few_samples_fail_the_latency_bound() {
    let quiet = witness(10_000, 0..0);
    let ok = [sample(0, 20.0), sample(500, 35.0), sample(1_000, 80.0)];
    assert!(judge_latency(&ok, &quiet, 3, 1_000.0).is_ok());
    let slow = [sample(0, 20.0), sample(500, 4_000.0), sample(5_000, 80.0)];
    assert!(judge_latency(&slow, &quiet, 3, 1_000.0).is_err());
    assert!(judge_latency(&ok[..1], &quiet, 3, 1_000.0).is_err());
}

#[test]
fn a_slow_answer_the_witness_shares_is_the_runners_stall_and_one_it_does_not_is_not() {
    let slow = [sample(0, 20.0), sample(1_000, 7_000.0), sample(9_000, 80.0)];
    // The witness is silent from 1.5 s to 7.5 s: it overlaps the slow request.
    let stalled = witness(10_000, 1_500..7_500);
    let ok = judge_latency(&slow, &stalled, 3, 3_000.0).unwrap();
    assert!((ok.worst_ms - 7_000.0).abs() < f64::EPSILON);
    // A stall elsewhere does not excuse it.
    let elsewhere = witness(10_000, 8_000..8_500);
    assert!(judge_latency(&slow, &elsewhere, 3, 3_000.0).is_err());
    let before = witness(10_000, 100..800);
    assert!(judge_latency(&slow, &before, 3, 3_000.0).is_err());
}

#[test]
fn every_round_must_exit_zero_in_all_three_jobs_and_there_must_be_rounds() {
    let good = "noise\nROUND cpu=0 io=0 net=0\nROUND cpu=0 io=0 net=0\n";
    assert_eq!(judge_rounds(good, 2).unwrap(), 2);
    let net_failed = "ROUND cpu=0 io=0 net=0\nROUND cpu=0 io=0 net=22\n";
    assert!(judge_rounds(net_failed, 1).unwrap_err().contains("net=22"));
    assert!(judge_rounds("", 1).is_err());
    assert!(judge_rounds(good, 3).is_err());
}

#[test]
fn a_listing_line_with_a_fourth_field_or_a_bad_size_is_ignored() {
    let tree = parse_tree("obj/a.gz 1.5 100 extra\nobj/b.gz 1.5 many\nobj/c.gz 1.5 7\n");
    assert_eq!(tree.keys().collect::<Vec<_>>(), ["obj/c.gz"]);
}

#[test]
fn a_drive_that_holds_one_percent_more_after_a_round_fails_and_a_small_move_passes() {
    let size = 100_000_000_000;
    assert!(judge_disk_back(40_000_000_000, 40_000_500_000, size).is_ok());
    assert!(judge_disk_back(40_000_000_000, 39_500_000_000, size).is_ok());
    // A volume left behind by a delete: 2 GB more.
    assert!(judge_disk_back(40_000_000_000, 42_000_000_000, size).is_err());
    assert!(judge_disk_back(1, 1, 0).is_err());
}

#[test]
fn memory_ten_percent_up_passes_and_eleven_fails() {
    assert!(judge_rss_back(1_000, 1_100).is_ok());
    assert!(judge_rss_back(1_000, 1_101).is_err());
    assert!(judge_rss_back(1_000, 400).is_ok());
    assert!(judge_rss_back(0, 5).is_err());
}
