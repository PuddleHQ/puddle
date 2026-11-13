// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared helpers for tests. A test that waits for something names what it waits for and says
//! how long it may take; a wait that runs out panics with that name, before nextest's
//! slow-timeout kills the test without one.
//!
//! This crate is a dev-dependency only: `scripts/quality-gate.py` fails any manifest that lists it
//! under `[dependencies]` or `[build-dependencies]` (the test-only crates puddle-e2e and
//! puddle-vm-tests excepted).
#![forbid(unsafe_code)]
#![expect(
    clippy::panic,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a panic that names the wait or the failed setup is how a helper fails a test"
)]

mod cpu;
mod events;
mod resources;
#[cfg(unix)]
mod script;
mod token;
mod wait;

pub use cpu::cpu_time;
pub use events::{
    EventSource, next_event_matching, next_event_within, no_event_for, no_event_matching,
};
pub use resources::{
    ReservedPort, TestDir, accepts, bound_port, http_status, private_tempdir, quiet_port,
};
#[cfg(unix)]
pub use script::{feed_stdin, write_executable, write_executable_bytes};
pub use token::{
    CHECKSUMMED_ENTROPY_LEN, TokenKind, entropy, jwt, token, with_broken_checksum, with_checksum,
};
pub use wait::{
    MAX_WAIT, Outcome, Step, eventually, eventually_async, eventually_blocking, returns_within,
};
