// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests over real loopback HTTP (tier L2): the guard's refusals,
//! every route against a real store, and the SSE stream.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

mod common;

mod credentials;
mod doctor;
mod endpoints;
mod environment;
pub(crate) mod events;
mod first_run;
mod guard;
mod identities;
mod logins;
mod network_health;
mod ports;
mod problems;
mod repos;
mod repositories;
mod routes;
mod rule_sets;
#[cfg(windows)]
mod second_user;
mod settings;
mod shell_init;
mod ui;
mod workspaces;
