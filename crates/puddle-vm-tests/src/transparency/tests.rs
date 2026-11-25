// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn step(name: &str, secs: f64) -> StepTime {
    StepTime {
        name: name.to_owned(),
        rc: 0,
        secs,
        bytes: if name == "download" {
            92 * 1024 * 1024
        } else {
            0
        },
        suspicious: 0,
    }
}

/// A run where every step takes `secs` times what a host does (host: 4 s, 40 s, 20 s, 15 s; the
/// download 4 s).
fn run(scale: f64) -> Vec<StepTime> {
    [
        ("download", 4.0),
        ("npm_cold", 40.0),
        ("npm_warm", 20.0),
        ("cargo_fetch", 15.0),
    ]
    .iter()
    .map(|(n, s)| step(n, s * scale))
    .collect()
}

fn comparison(guest_scale: f64) -> Comparison {
    Comparison::new(vec![run(1.0), run(1.1)], run(guest_scale))
}

#[test]
fn a_workspace_as_fast_as_the_host_and_one_at_just_under_twice_pass() {
    assert_eq!(comparison(1.0).problems(), Vec::<String>::new());
    assert_eq!(comparison(1.9).problems(), Vec::<String>::new());
}

#[test]
fn a_workspace_three_times_slower_fails_every_step_it_slows() {
    // The breakage the bar names: a throttle on the path between the tool and the registry.
    let problems = comparison(3.0).problems();
    for name in STEPS {
        assert!(
            problems.iter().any(|p| p.starts_with(&format!("{name}:"))),
            "{name} not flagged: {problems:#?}"
        );
    }
    assert!(
        problems.iter().any(|p| p.contains("MiB/s")),
        "{problems:#?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("ratio 3.00")),
        "{problems:#?}"
    );
}

#[test]
fn a_download_at_half_the_rate_is_the_edge_and_a_slower_one_fails() {
    assert_eq!(
        comparison(2.0)
            .problems()
            .iter()
            .filter(|p| p.starts_with("download"))
            .count(),
        0
    );
    let slow = comparison(2.2).problems();
    assert!(
        slow.iter()
            .any(|p| p.starts_with("download: ") && p.contains("MiB/s")),
        "{slow:#?}"
    );
}

#[test]
fn the_ratio_is_against_the_hosts_better_run_and_a_short_host_step_has_a_floor() {
    let mut fast = run(1.0);
    fast[2].secs = 0.4; // a host warm install of 0.4 s counts as the 2 s floor
    let mut busy = run(1.0);
    busy[2].secs = 30.0;
    let mut guest = run(1.0);
    guest[2].secs = 3.9;
    assert_eq!(
        Comparison::new(vec![fast.clone(), busy.clone()], guest.clone()).problems(),
        Vec::<String>::new()
    );
    guest[2].secs = 4.1;
    let problems = Comparison::new(vec![fast, busy], guest).problems();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    assert!(problems[0].starts_with("npm_warm: 4.1 s"), "{problems:#?}");
}

#[test]
fn a_failed_missing_or_retrying_step_is_a_problem_and_so_is_a_baseline_that_failed() {
    let mut guest = run(1.0);
    guest[1].rc = 1;
    guest[2].suspicious = 3;
    guest.pop();
    let problems = Comparison::new(vec![run(1.0), run(1.0)], guest).problems();
    assert!(
        problems
            .iter()
            .any(|p| p == "npm_cold: failed in the workspace (exit 1)"),
        "{problems:#?}"
    );
    assert!(
        problems.iter().any(|p| p.starts_with("npm_warm: 3 lines")),
        "{problems:#?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p == "cargo_fetch: the workspace run has no such step"),
        "{problems:#?}"
    );
    let mut broken = run(1.0);
    broken[0].rc = 6;
    let problems = Comparison::new(vec![broken, run(1.0)], run(1.0)).problems();
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("download: the host baseline")),
        "{problems:#?}"
    );
    let one_run = Comparison::new(vec![run(1.0)], run(1.0)).problems();
    assert_eq!(
        one_run.len(),
        STEPS.len(),
        "a single host run is no baseline: {one_run:#?}"
    );
}

#[test]
fn a_download_that_moved_other_bytes_than_the_host_is_a_problem() {
    let mut guest = run(1.0);
    guest[0].bytes -= 1;
    let problems = Comparison::new(vec![run(1.0), run(1.0)], guest).problems();
    assert!(
        problems
            .iter()
            .any(|p| p.starts_with("download: ") && p.contains("bytes in the workspace")),
        "{problems:#?}"
    );
}

#[test]
fn step_lines_parse_and_other_lines_are_ignored() {
    let out = "noise\nTSTEP name=npm_cold rc=0 secs=41.25 bytes=0 suspicious=2\nTSTEP-LOG x\nTSTEP name=download rc=0 secs=3.50 bytes=96468992 suspicious=0\n";
    let got = parse_steps(out).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        got[0],
        StepTime {
            name: "npm_cold".into(),
            rc: 0,
            secs: 41.25,
            bytes: 0,
            suspicious: 2
        }
    );
    assert_eq!(got[1].bytes, 96_468_992);
    assert!(parse_steps("TSTEP name=x rc=0 secs=oops bytes=0 suspicious=0").is_err());
    assert!(parse_steps("TSTEP name=x rc=0 secs=1").is_err());
    assert!(parse_steps("TSTEP-ERROR odd scratch folder /").is_err());
}

fn created(secs: &[(&str, f64)]) -> Vec<(String, f64)> {
    secs.iter().map(|(n, s)| ((*n).to_owned(), *s)).collect()
}

#[test]
fn a_create_inside_its_ceilings_passes() {
    let steps = created(&[
        ("preparing_volume", 3.0),
        ("pulling_image", 90.0),
        ("starting", 8.0),
        ("syncing", 2.0),
        ("done", 0.0),
    ]);
    assert_eq!(create_problems(&steps), Vec::<String>::new());
}

#[test]
fn a_create_step_that_stalls_a_new_step_and_a_long_total_each_fail() {
    let stalled = created(&[
        ("preparing_volume", 3.0),
        ("pulling_image", 90.0),
        ("starting", 121.0),
        ("done", 0.0),
    ]);
    assert_eq!(
        create_problems(&stalled),
        ["starting took 121.0 s, over its ceiling of 120 s"]
    );
    let new = created(&[("warming_up", 1.0), ("done", 0.0)]);
    assert_eq!(
        create_problems(&new),
        ["warming_up is a create step with no ceiling"]
    );
    let long = created(&[
        ("pulling_image", 410.0),
        ("starting", 110.0),
        ("syncing", 55.0),
        ("cloning", 200.0),
        ("done", 0.0),
    ]);
    assert_eq!(
        create_problems(&long),
        ["the create took 775.0 s, over 600 s"]
    );
}

#[test]
fn the_script_is_confined_to_its_own_scratch_folder() {
    assert!(STEPS_SCRIPT.contains("trap 'rm -rf \"$W\"' EXIT"));
    assert_eq!(
        STEPS_SCRIPT.matches("rm -rf").count(),
        2,
        "only the trap and the warm install remove anything"
    );
    for name in STEPS {
        assert!(STEPS_SCRIPT.contains(&format!("step {name} ")), "{name}");
    }
}
