// SPDX-License-Identifier: AGPL-3.0-or-later
use super::plumbing::PullThrottle;
use puddle_compute::{PullPhase, PullProgress};

fn at(phase: PullPhase, done: u64, total: Option<u64>) -> PullProgress {
    PullProgress { phase, done, total }
}

#[test]
fn a_pull_is_reported_once_per_whole_percent_and_again_for_each_new_phase() {
    let t = PullThrottle::default();
    let d = PullPhase::Downloading;
    assert!(t.lets_through(at(d, 0, Some(1000))));
    assert!(!t.lets_through(at(d, 5, Some(1000))), "still 0 %");
    assert!(t.lets_through(at(d, 10, Some(1000))));
    assert!(!t.lets_through(at(d, 19, Some(1000))));
    assert!(t.lets_through(at(d, 1000, Some(1000))));
    assert!(t.lets_through(at(PullPhase::Unpacking, 0, None)));
    assert!(!t.lets_through(at(PullPhase::Unpacking, 1024, None)));
    assert!(t.lets_through(at(PullPhase::Unpacking, 9 * 1024 * 1024, None)));
}
