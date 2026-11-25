// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a create tells the card while the image downloads.

use puddle_compute::{PullPhase, PullProgress};

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_downloads_its_image_reports_the_bytes_and_then_the_unpack() {
    let rig = Rig::named_ids();
    let mib = 1024 * 1024;
    rig.runtime.script_pull(vec![
        PullProgress {
            phase: PullPhase::Downloading,
            done: 0,
            total: Some(100 * mib),
        },
        PullProgress {
            phase: PullPhase::Downloading,
            done: 40 * mib,
            total: Some(100 * mib),
        },
        PullProgress {
            phase: PullPhase::Downloading,
            done: 100 * mib,
            total: Some(100 * mib),
        },
        PullProgress {
            phase: PullPhase::Unpacking,
            done: 0,
            total: None,
        },
    ]);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    let counts: Vec<(String, Option<u64>, Option<u64>)> = events
        .seen
        .iter()
        .filter(|e| e["type"] == "workspace_progress" && !e["bytes_done"].is_null())
        .map(|e| {
            (
                e["step"].as_str().unwrap().to_owned(),
                e["bytes_done"].as_u64(),
                e["bytes_total"].as_u64(),
            )
        })
        .collect();
    assert_eq!(
        counts,
        [
            ("pulling_image".to_owned(), Some(0), Some(100 * mib)),
            ("pulling_image".to_owned(), Some(40 * mib), Some(100 * mib)),
            ("pulling_image".to_owned(), Some(100 * mib), Some(100 * mib)),
            ("unpacking_image".to_owned(), Some(0), None),
        ]
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_does_not_announce_an_image_pull() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    rig.runtime.script_pull(vec![PullProgress {
        phase: PullPhase::Downloading,
        done: 1,
        total: Some(2),
    }]);
    let before = events.seen.len();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        events.seen[before..]
            .iter()
            .all(|e| e["step"] != "pulling_image" && e["step"] != "unpacking_image"),
        "{:?}",
        &events.seen[before..]
    );
    host.shutdown().await;
}
