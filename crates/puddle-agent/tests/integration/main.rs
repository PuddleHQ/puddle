// SPDX-License-Identifier: AGPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

#![expect(
    unreachable_pub,
    reason = "the shared harness and the path-included fuzz checks are pub for their sibling modules and the fuzz crate; this is a test binary"
)]

#[cfg(unix)]
mod common;

mod bridge;
mod capture;
mod editor;
mod editor_stream;
mod forward;
mod fuzz_corpus;
mod host_control;
mod merge_file;
mod microsoft_server;
mod mise_fetch;
mod mise_install;
mod oom;
mod real_bridge;
mod relay;
mod ssh_agent;
mod ssh_refusal;
mod version;
