// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn req(connects: u32, seconds: f64) -> Request {
    Request {
        http_code: 200,
        connects,
        seconds,
        exit: 0,
    }
}

#[test]
fn the_script_sends_every_request_from_one_curl_without_parallelism() {
    let s = script(
        "http://h.invalid/keepalive/16",
        1000,
        Route::EnvProxyTunnel,
        120,
    );
    assert!(s.contains("seq 1 1000"));
    assert!(s.contains("'http://h.invalid/keepalive/16'"));
    assert!(s.contains("--proxytunnel"));
    assert!(s.contains("--max-time 120"));
    assert!(
        !s.contains(" -Z"),
        "parallel transfers would not share a connection in order"
    );
    assert_eq!(s.matches("curl ").count(), 1);
    assert!(s.trim_end().ends_with("exit 0"));
    assert!(script("http://x/y", 2, Route::Direct, 5).contains("--noproxy '*'"));
}

#[test]
fn requests_come_from_keepalive_lines_and_a_malformed_one_names_itself() {
    let out = "noise\nKEEPALIVE 200 1 0.030 0\nKEEPALIVE 200 0 0.0005 0\n";
    let got = parse_requests(out).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0], req(1, 0.030));
    assert_eq!(got[1].connects, 0);
    for bad in [
        "KEEPALIVE 200 1 2",
        "KEEPALIVE 200 1 2 3 4",
        "KEEPALIVE a 1 2 3",
    ] {
        assert!(parse_requests(bad).unwrap_err().contains(bad), "{bad}");
    }
}

#[test]
fn the_cost_is_the_mean_of_the_requests_after_the_first() {
    let r = Report::new(3, vec![req(1, 0.500), req(0, 0.002), req(0, 0.004)]);
    assert!((r.per_request_ms().unwrap() - 3.0).abs() < 1e-9);
    assert!((r.slowest_ms() - 4.0).abs() < 1e-9);
    assert_eq!(r.problems(), Vec::<String>::new());
    assert!(Report::new(1, vec![req(1, 0.1)]).per_request_ms().is_none());
}

#[test]
fn a_second_connection_a_short_count_and_a_failed_request_are_problems() {
    let two = Report::new(2, vec![req(1, 0.1), req(1, 0.1)]).problems();
    assert_eq!(two.len(), 1);
    assert!(two[0].contains("2 connections"), "{two:?}");
    let short = Report::new(3, vec![req(1, 0.1)]).problems();
    assert!(
        short[0].contains("1 requests reported, 3 sent"),
        "{short:?}"
    );
    let failed = Report::new(
        2,
        vec![
            req(1, 0.1),
            Request {
                http_code: 0,
                connects: 0,
                seconds: 0.1,
                exit: 28,
            },
        ],
    )
    .problems();
    assert!(
        failed[0].contains("1 requests did not end in a 200"),
        "{failed:?}"
    );
}
