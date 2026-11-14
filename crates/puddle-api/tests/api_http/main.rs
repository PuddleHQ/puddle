// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests over real loopback HTTP (tier L2): the guard's refusals,
//! every route against a real store, and the SSE stream.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

mod app_icon;
mod common;

mod admin;
mod credentials;
mod doctor;
mod endpoints;
mod environment;
pub(crate) mod events;
mod first_run;
mod guard;
mod identities;
mod interception;
mod logins;
mod network_health;
mod orphan_volumes;
mod ports;
mod problems;
mod proxy_sign_in;
mod remote;
mod repos;
mod repositories;
mod repository_hosts;
mod request_rules;
mod routes;
mod rule_sets;
#[cfg(windows)]
mod second_user;
mod settings;
mod shell_init;
mod sign_in;
mod ssh_agent;
mod terminals;
mod ui;
mod workspaces;
