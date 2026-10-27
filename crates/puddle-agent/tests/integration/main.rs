// SPDX-License-Identifier: GPL-3.0-or-later
//! One test binary for every area below, so the shared support code compiles and links once.

#![expect(
    unreachable_pub,
    reason = "the shared harness is pub for its sibling modules; this is a test binary"
)]

mod common;

mod bridge;
mod capture;
mod editor;
mod forward;
mod host_control;
mod merge_file;
mod oom;
mod real_bridge;
mod relay;
mod ssh_refusal;
mod version;
