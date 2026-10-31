// SPDX-License-Identifier: GPL-3.0-or-later
//! The DNS message the guest's stub reads from a tool in the guest. The checks are in
//! `dns_message` in `fuzz/checks/dns_message.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../checks/dns_message.rs"]
mod checks;

fuzz_target!(|data: &[u8]| checks::dns_message(data));
