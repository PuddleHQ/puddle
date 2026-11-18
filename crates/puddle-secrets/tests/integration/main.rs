// SPDX-License-Identifier: AGPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

mod common;
mod fake_log;

mod discovery;
mod fixture_profile;
mod gcm;
mod login_session;
mod real_store;
mod real_tools;
mod signin;
mod sources;
mod vault_key;
