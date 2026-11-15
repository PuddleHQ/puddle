// SPDX-License-Identifier: AGPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

#[path = "../common/mod.rs"]
mod common;

mod checkouts_sh;
mod clear_locks_sh;
mod delete_check_sh;
mod git_hook_env;
mod prepare_sh;
