// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ok(bytes: u64) -> Transfer {
    Transfer {
        http_code: 200,
        bytes,
        seconds: 1.5,
        exit: 0,
    }
}

#[test]
fn the_script_names_the_url_count_route_and_stall_bar() {
    let tunnel = script("http://h.invalid/gate/7", 256, Route::EnvProxyTunnel, 120);
    assert!(tunnel.contains("seq 1 256"));
    assert!(tunnel.contains("'http://h.invalid/gate/7'"));
    assert!(tunnel.contains("--parallel-max 256"));
    assert!(tunnel.contains("--proxytunnel"));
    assert!(tunnel.contains("--speed-limit 1 --speed-time 5 "));
    assert!(tunnel.contains("--max-time 120"));
    assert!(tunnel.trim_end().ends_with("exit 0"));
    let direct = script("http://127.0.0.1:1/x", 2, Route::Direct, 30);
    assert!(direct.contains("--noproxy '*'"));
    assert!(!direct.contains("--proxytunnel"));
}

#[test]
fn transfers_come_from_result_lines_and_other_lines_are_ignored() {
    let out = "curl: (28) Operation too slow\nRESULT 200 2097152 1.25 0\nnoise RESULT 1 2 3 4\nRESULT 0 65536 5.001 28\n";
    let got = parse_transfers(out).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        Transfer {
            http_code: 200,
            bytes: 2_097_152,
            seconds: 1.25,
            exit: 0
        }
    );
    assert_eq!(got[1].exit, 28);
    assert_eq!(parse_transfers("").unwrap(), vec![]);
}

#[test]
fn a_malformed_result_line_is_an_error_naming_it() {
    for bad in [
        "RESULT 200 1 2",
        "RESULT 200 1 2 3 4",
        "RESULT x 1 2 3",
        "RESULT 200 -1 2 3",
        "RESULT 200 1 two 3",
        "RESULT 200 1 2 z",
    ] {
        let err = parse_transfers(bad).unwrap_err();
        assert!(err.contains("unreadable result line"), "{bad}: {err}");
    }
}

#[test]
fn a_clean_run_has_no_problems_and_a_summary() {
    let report = Report::new(
        3,
        1_048_576,
        vec![ok(1_048_576), ok(1_048_576), ok(1_048_576)],
    );
    assert_eq!(report.problems(), Vec::<String>::new());
    assert_eq!(report.completed(), 3);
    assert_eq!(report.stalled(), 0);
    assert_eq!(report.moved_bytes(), 3 * 1_048_576);
    assert!((report.slowest_secs() - 1.5).abs() < 1e-9);
    assert_eq!(
        report.summary(2.0),
        "3 of 3 transfers completed, 0 stalled, slowest 1.5 s, 3 MiB in 2.0 s wall (1.5 MiB/s)"
    );
}

#[test]
fn a_stalled_transfer_fails_the_bar() {
    let stalled = Transfer {
        http_code: 200,
        bytes: 65_536,
        seconds: 6.0,
        exit: 28,
    };
    let report = Report::new(2, 1_048_576, vec![ok(1_048_576), stalled]);
    let problems = report.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("1 transfers went 5 s without progress"));
    assert_eq!(report.completed(), 1);
    assert_eq!(report.stalled(), 1);
    assert!((report.slowest_secs() - 6.0).abs() < 1e-9);
}

#[test]
fn a_missing_short_or_refused_transfer_fails_the_bar() {
    let short = Transfer {
        http_code: 200,
        bytes: 10,
        seconds: 1.0,
        exit: 0,
    };
    let refused = Transfer {
        http_code: 403,
        bytes: 0,
        seconds: 0.1,
        exit: 0,
    };
    let reset = Transfer {
        http_code: 200,
        bytes: 5,
        seconds: 1.0,
        exit: 56,
    };
    let report = Report::new(5, 100, vec![ok(100), short, refused, reset]);
    let problems = report.problems();
    assert!(
        problems[0].contains("4 transfers reported, 5 started"),
        "{problems:?}"
    );
    assert_eq!(problems.len(), 4, "{problems:?}");
    assert!(
        problems
            .iter()
            .any(|p| p.contains("http 403, 0 of 100 bytes, curl exit 0"))
    );
    assert!(problems.iter().any(|p| p.contains("curl exit 56")));
    assert_eq!(report.completed(), 1);
}

#[test]
fn only_five_failures_are_listed() {
    let bad = Transfer {
        http_code: 500,
        bytes: 0,
        seconds: 0.1,
        exit: 0,
    };
    let report = Report::new(9, 1, vec![bad; 9]);
    assert_eq!(report.problems().len(), 5);
}

#[test]
fn an_empty_run_summarises_without_dividing_by_zero() {
    let report = Report::new(1, 1, vec![]);
    assert!(
        report
            .summary(0.0)
            .starts_with("0 of 1 transfers completed")
    );
    assert!((report.slowest_secs()).abs() < f64::EPSILON);
}
