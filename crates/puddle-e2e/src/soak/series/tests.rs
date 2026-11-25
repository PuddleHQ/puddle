// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let s =
        Series::parse("1\tproc.a.1.rss_kb\t1\n1\tproc.a.1.handles\t1\n1\tapi.x.ms\t1\n").unwrap();
    let got: Vec<_> = s.metrics("proc.", ".rss_kb").collect();
    assert_eq!(got, ["proc.a.1.rss_kb"]);
}
