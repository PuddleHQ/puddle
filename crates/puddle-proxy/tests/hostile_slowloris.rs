// SPDX-License-Identifier: GPL-3.0-or-later
//! Ten thousand streams on one route, spread over a hundred agent sessions, each stop halfway
//! through a request head and stay silent (slowloris). The workspace's cap of 1000 holds: the first
//! 1000 are cut off at the head deadline with a `408` that names it, every other stream is refused
//! with a `503` that names the cap, and nothing waits for the default 30 s. A second workspace on
//! the same proxy finishes 64 connections in under 5 s meanwhile.

mod hostile_support;

use std::time::{Duration, Instant};

use hostile_support::{Guest, bystander_round, rig_with};
use puddle_proxy::ProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CAP: usize = 1000;
const SESSIONS: usize = 100;
const PER_SESSION: usize = 100;
const DEADLINE: Duration = Duration::from_secs(3);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg11_ten_thousand_stalled_heads_are_cut_off_at_the_deadline_and_over_the_cap_refused()
 {
    let config = ProxyConfig::default()
        .with_max_streams_per_workspace(CAP)
        .with_head_timeout(DEADLINE);
    let rig = rig_with(config).await;
    let mut guests = Vec::new();
    for _ in 0..SESSIONS {
        guests.push(Guest::connect(&rig.attacked).await);
    }
    let started = Instant::now();
    let mut tasks = Vec::new();
    for guest in &mut guests {
        for _ in 0..PER_SESSION {
            let mut stream = guest.control.open_stream().await.unwrap();
            tasks.push(tokio::spawn(async move {
                stream
                    .write_all(b"CONNECT slow.test:443 HTTP/1.1\r\nX-Slow: ")
                    .await
                    .ok()?;
                let mut raw = Vec::new();
                tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
                    .await
                    .ok()?
                    .ok()?;
                Some(String::from_utf8_lossy(&raw).into_owned())
            }));
        }
    }
    let bystander_took = bystander_round(&rig.bystander, rig.echo).await;
    let (mut cut_off, mut refused, mut other) = (0, 0, Vec::new());
    for task in tasks {
        match task.await.unwrap() {
            Some(text)
                if text.starts_with("HTTP/1.1 408") && text.contains("not received within 3s") =>
            {
                cut_off += 1;
            }
            Some(text)
                if text.starts_with("HTTP/1.1 503")
                    && text
                        .contains("too many open connections from this workspace (limit 1000)") =>
            {
                refused += 1;
            }
            answer => other.push(answer),
        }
    }
    let elapsed = started.elapsed();
    assert_eq!(
        (cut_off, refused, other.len()),
        (CAP, SESSIONS * PER_SESSION - CAP, 0),
        "{:?}",
        other.iter().take(3).collect::<Vec<_>>()
    );
    // The deadline is what ended them: not at once, and nowhere near the 30 s default.
    assert!(
        elapsed >= DEADLINE && elapsed < Duration::from_secs(15),
        "{elapsed:?}"
    );
    assert!(
        bystander_took < Duration::from_secs(5),
        "the other workspace's 64 connections took {bystander_took:?}"
    );
}
