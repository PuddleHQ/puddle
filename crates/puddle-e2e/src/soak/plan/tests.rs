// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_full_day_is_twelve_real_hours_and_the_hosted_one_fits_a_six_hour_job() {
    assert_eq!(FULL.active() + FULL.idle(), Duration::from_hours(12));
    let hosted = HOSTED.active() + HOSTED.idle();
    assert_eq!(hosted, Duration::from_mins(4 * 60 + 30));
    assert_eq!(SMOKE.active() + SMOKE.idle(), Duration::from_mins(10));
}

#[test]
fn a_playbook_minute_scales_with_the_hour() {
    assert_eq!(FULL.at(150), Duration::from_mins(150));
    assert_eq!(HOSTED.at(60), Duration::from_secs(1350));
    assert_eq!(SMOKE.at(30), Duration::from_secs(30));
    assert_eq!(FULL.sample_every(), Duration::from_mins(5));
    assert_eq!(SMOKE.sample_every(), Duration::from_secs(5));
    assert_eq!(SMOKE.sampler_alone(), Duration::from_secs(15));
}

#[test]
fn profiles_are_found_by_name() {
    assert_eq!(Profile::named("hosted"), Some(HOSTED));
    assert_eq!(Profile::named("nope"), None);
}

#[test]
fn the_day_has_the_playbooks_events_in_order() {
    let day = events(FULL, true);
    let at = |e: Event| -> Vec<u64> {
        day.iter()
            .filter(|&&(_, x)| x == e)
            .map(|&(m, _)| m)
            .collect()
    };
    assert_eq!(at(Event::Baseline), [0]);
    assert_eq!(at(Event::RestartPuddle), [120]);
    assert_eq!(at(Event::HardKill), [300]);
    assert_eq!(at(Event::InboxKeys), [420]);
    assert_eq!(at(Event::StopLoops), [480]);
    assert_eq!(at(Event::InboxBatch), [30, 90, 150, 210, 270, 330, 390]);
    assert!(day.windows(2).all(|w| w[0].0 <= w[1].0), "{day:?}");
    let ui: Vec<Event> = day
        .iter()
        .map(|&(_, e)| e)
        .filter(|e| e.in_the_ui())
        .collect();
    assert_eq!(
        ui,
        [
            Event::Activity,
            Event::Rules,
            Event::Flood,
            Event::InboxKeys
        ]
    );
}

#[test]
fn without_workloads_only_the_baseline_and_the_stop_remain() {
    assert_eq!(
        events(SMOKE, false),
        [(0, Event::Baseline), (480, Event::StopLoops)]
    );
}

#[test]
fn each_loop_starts_a_round_every_period_while_the_loops_run() {
    assert_eq!(rounds(FULL, Workload::Build).len(), 24);
    assert_eq!(rounds(FULL, Workload::Js).len(), 16);
    let agent = rounds(FULL, Workload::Agent);
    assert_eq!(agent.len(), 32);
    assert_eq!(agent.first(), Some(&0));
    assert_eq!(agent.last(), Some(&465));
}

#[test]
fn a_restart_cuts_the_rounds_it_ends_and_no_others() {
    for w in Workload::ALL {
        assert!(Event::RestartPuddle.cuts(w) && Event::HardKill.cuts(w));
        assert!(!Event::Backup.cuts(w) && !Event::InboxBatch.cuts(w));
    }
    assert!(Event::RestartJs.cuts(Workload::Js));
    assert!(!Event::RestartJs.cuts(Workload::Build) && !Event::RestartJs.cuts(Workload::Agent));
}

#[test]
fn a_round_is_cut_only_by_an_event_that_began_after_it_started() {
    let cuts = Cuts::default();
    let before = Instant::now();
    assert_eq!(cuts.cut(before, Workload::Js), None);
    cuts.began(Event::Backup);
    assert_eq!(cuts.cut(before, Workload::Js), None);
    cuts.began(Event::RestartJs);
    assert_eq!(cuts.cut(before, Workload::Build), None);
    assert_eq!(cuts.cut(before, Workload::Js), Some(Event::RestartJs));
    cuts.began(Event::HardKill);
    assert_eq!(cuts.cut(before, Workload::Build), Some(Event::HardKill));
    assert_eq!(cuts.cut(Instant::now(), Workload::Js), None);
}
