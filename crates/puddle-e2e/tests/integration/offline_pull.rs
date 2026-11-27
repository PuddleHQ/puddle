// SPDX-License-Identifier: AGPL-3.0-or-later
//! An image pull while the route to the registry is gone (QA playbook 07, S16 and the first half
//! of S17): the real `pull_image` of the msb adapter goes through Puddle's pull proxy to a lab
//! registry that sits behind a link a test can cut. A refused route and a route that swallows
//! every packet must each end the pull within a bound with a reason that names the registry and
//! says Puddle could not connect, never a bare timeout; and once the route is back, the same
//! runtime pulls the image with no restart.
#![cfg(target_os = "linux")]
#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::time::{Duration, Instant};

use puddle_compute::{ComputeError, Runtime};
use puddle_e2e::pull_lab::REGISTRY;

use crate::image_pull_resume::{Lab, lab_behind_a_link};

/// The longest a user waits for the message while the route refuses (measured: 28 s).
const BOUND: Duration = Duration::from_secs(40);

/// The same while the route swallows every packet: the pull gives up when no manifest came within
/// 20 s (it was 76 s: the proxy connect timeout once per try of the registry client).
const BLACKHOLE_BOUND: Duration = Duration::from_secs(30);

async fn failed_pull(lab: &Lab) -> (String, Duration) {
    let started = Instant::now();
    let err = lab.runtime.pull_image(&lab.image).await.unwrap_err();
    let took = started.elapsed();
    let ComputeError::ImagePull { reason, .. } = &err else {
        panic!("expected an image-pull error, got {err:?}");
    };
    (reason.clone(), took)
}

fn names_the_registry_and_the_failure(reason: &str) {
    assert!(
        reason.contains(REGISTRY),
        "the registry is not named: {reason}"
    );
    assert!(
        reason.contains("could not connect"),
        "the reason does not say Puddle could not connect: {reason}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_while_the_route_refuses_names_the_registry_and_comes_back_when_the_route_does() {
    let lab = lab_behind_a_link().await;
    let link = lab.link.as_ref().unwrap();
    link.cut().await;
    let (reason, took) = failed_pull(&lab).await;
    names_the_registry_and_the_failure(&reason);
    assert!(took < BOUND, "the message took {took:?}");

    link.restore().await;
    let config = lab.runtime.pull_image(&lab.image).await.unwrap();
    assert_eq!(config.env_var("PUDDLE_LAB"), Some("resume"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pull_while_the_route_swallows_every_packet_ends_within_the_bound_naming_the_registry() {
    let lab = lab_behind_a_link().await;
    let link = lab.link.as_ref().unwrap();
    link.blackhole().await;
    let (reason, took) = failed_pull(&lab).await;
    eprintln!("blackholed: {took:?}: {reason}");
    names_the_registry_and_the_failure(&reason);
    assert!(took < BLACKHOLE_BOUND, "the message took {took:?}");

    link.restore().await;
    lab.runtime.pull_image(&lab.image).await.unwrap();
}
