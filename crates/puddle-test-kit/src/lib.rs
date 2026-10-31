// SPDX-License-Identifier: GPL-3.0-or-later
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
mod wait;

pub use cpu::cpu_time;
pub use events::{
    EventSource, next_event_matching, next_event_within, no_event_for, no_event_matching,
};
pub use resources::{ReservedPort, TestDir, accepts, bound_port};
pub use wait::{MAX_WAIT, Outcome, Step, eventually, eventually_async, eventually_blocking};
