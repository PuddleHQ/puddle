// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the start-up hash check costs on this machine: the msb binary and the firmware library of
//! the Windows set (48 MiB and 22 MiB), hashed with `sha256_file`, the function `open` uses.
//! Ignored because it measures and asserts nothing about time (a wall-clock bound would flake);
//! run it by hand where the number matters, for example on a `windows-2025` runner:
//! `cargo test -p puddle-runtime --release --test digest_cost -- --ignored --nocapture`.

use std::time::Instant;

use puddle_runtime::{HostOs, sha256_file};

const MSB_BYTES: usize = 48 << 20;
const FIRMWARE_BYTES: usize = 22 << 20;
const RUNS: usize = 7;

/// Bytes that no compressing disk layer can shrink and that differ per file.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[test]
#[ignore = "a measurement for people to read, not a test"]
fn hashing_the_windows_runtime_set_costs() {
    let dir = tempfile::tempdir().unwrap();
    let msb = dir.path().join(HostOs::Windows.runtime_files().msb);
    let firmware = dir.path().join(HostOs::Windows.runtime_files().libkrunfw);
    std::fs::write(&msb, noise(MSB_BYTES, 0x9E37_79B9_7F4A_7C15)).unwrap();
    std::fs::write(&firmware, noise(FIRMWARE_BYTES, 0x1357_9BDF_2468_ACE1)).unwrap();

    let mut millis = Vec::new();
    let mut hashes = Vec::new();
    for _ in 0..RUNS {
        let start = Instant::now();
        let both = (sha256_file(&msb).unwrap(), sha256_file(&firmware).unwrap());
        millis.push(start.elapsed().as_secs_f64() * 1000.0);
        hashes.push(both);
    }
    assert!(
        hashes.windows(2).all(|pair| pair[0] == pair[1]),
        "the same files must give the same hashes in every run"
    );
    // The first pass is the one a real start sees right after install; the rest are warm.
    let first = millis[0];
    let mut warm = millis[1..].to_vec();
    warm.sort_by(f64::total_cmp);
    {
        eprintln!(
            "digest cost: {} MiB, first {first:.0} ms, warm min {:.0} ms, median {:.0} ms, max {:.0} ms ({} warm runs)",
            (MSB_BYTES + FIRMWARE_BYTES) >> 20,
            warm[0],
            warm[warm.len() / 2],
            warm[warm.len() - 1],
            warm.len()
        );
    }
}
