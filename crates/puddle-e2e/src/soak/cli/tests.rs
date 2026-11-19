// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

async fn judge_file(text: &str, extra: &[&str]) -> (Result<u8, String>, String) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("metrics.tsv");
    std::fs::write(&path, text).unwrap();
    let mut a = vec!["judge", path.to_str().unwrap()];
    a.extend_from_slice(extra);
    let mut out = Vec::new();
    let code = run(&args(&a), &mut out).await;
    (code, String::from_utf8(out).unwrap())
}

/// A run whose RSS steps up 40 % at hour 2.
fn leaking() -> String {
    let mut s = Series::default();
    for i in 0..=(8 * 12) {
        let t = 5000 + i * 300;
        s.push(
            t,
            "proc.w.1.rss_kb",
            if i < 24 { 100_000.0 } else { 140_000.0 },
        );
    }
    s.render()
}

#[tokio::test]
async fn judge_exits_1_with_the_finding_on_a_major_and_0_on_a_quiet_run() {
    let (code, out) = judge_file(&leaking(), &[]).await;
    assert_eq!(code, Ok(1));
    assert!(out.contains("MAJOR Leak proc.w.1.rss_kb"), "{out}");
    assert!(out.contains("the run fails"));
    let (code, out) = judge_file("5000\tproc.w.1.rss_kb\t1\n", &["--start", "5000"]).await;
    assert_eq!(code, Ok(0));
    assert!(out.contains("0 finding(s), no MAJOR"), "{out}");
}

#[tokio::test]
async fn judge_reads_the_start_and_the_active_hours_from_its_flags() {
    // Only 3 active hours: hour 8 is the end of the data, so the step is a rise to hour 3's level.
    let (code, _) = judge_file(&leaking(), &["--active-hours", "3"]).await;
    assert_eq!(code, Ok(1));
    let (code, _) = judge_file(&leaking(), &["--start", "x"]).await;
    assert_eq!(code.unwrap_err(), "--start: `x` is not a number");
}

#[tokio::test]
async fn bad_input_is_a_usage_error() {
    assert!(
        run(&args(&[]), &mut Vec::new())
            .await
            .unwrap_err()
            .starts_with("usage")
    );
    assert!(
        run(&args(&["judge"]), &mut Vec::new())
            .await
            .unwrap_err()
            .starts_with("usage")
    );
    assert!(
        run(&args(&["judge", "/nonexistent/m.tsv"]), &mut Vec::new())
            .await
            .is_err()
    );
    let (code, _) = judge_file("garbage\n", &[]).await;
    assert!(code.unwrap_err().starts_with("metrics line 1"));
    let (code, _) = judge_file("# nothing\n", &[]).await;
    assert_eq!(code.unwrap_err(), "the file holds no measurement");
    assert_eq!(
        run(&args(&["judge", "a", "--start"]), &mut Vec::new())
            .await
            .unwrap_err(),
        "--start needs a value"
    );
    assert_eq!(
        run(&args(&["sample"]), &mut Vec::new()).await.unwrap_err(),
        "--out is required"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn sample_appends_one_sample_per_round_to_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("metrics.tsv");
    let a = args(&[
        "sample",
        "--out",
        out.to_str().unwrap(),
        "--api",
        "127.0.0.1:1",
        "--interval",
        "1",
        "--count",
        "2",
    ]);
    assert_eq!(run(&a, &mut Vec::new()).await, Ok(0));
    let series = Series::parse(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(series.points("host.free_ram_kb").len(), 2);
    let bad = args(&[
        "sample",
        "--out",
        out.to_str().unwrap(),
        "--api",
        "a",
        "--file",
        "novalue",
    ]);
    assert_eq!(
        run(&bad, &mut Vec::new()).await.unwrap_err(),
        "--file needs NAME=PATH"
    );
}
