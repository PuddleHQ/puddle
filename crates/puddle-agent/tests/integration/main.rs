// SPDX-License-Identifier: GPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

#![cfg_attr(
    unix,
    expect(
        unreachable_pub,
        reason = "the shared harness is pub for its sibling modules; this is a test binary"
    )
)]

#[cfg(unix)]
mod common;

mod bridge;
mod capture;
mod editor;
mod editor_stream;
mod forward;
mod host_control;
mod merge_file;
mod microsoft_server;
mod oom;
mod real_bridge;
mod relay;
mod ssh_refusal;
mod version;
