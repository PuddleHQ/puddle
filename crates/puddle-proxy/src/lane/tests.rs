// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn a_job_runs_off_the_calling_thread_and_returns() {
    let lane = Lane::new();
    let caller = std::thread::current().id();
    let ran_on = lane.run(|| std::thread::current().id()).await.unwrap();
    assert_ne!(ran_on, caller);
}

#[tokio::test]
async fn a_panicking_job_is_reported_and_the_lane_keeps_working() {
    let lane = Lane::new();
    assert!(lane.run(|| panic!("boom")).await.is_err());
    assert_eq!(lane.run(|| 7).await.unwrap(), 7);
}
