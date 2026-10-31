// SPDX-License-Identifier: GPL-3.0-or-later
//! The DNS message the guest's stub reads from a tool in the guest. The checks are in
//! `puddle_agent::fuzzing::dns_message`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| puddle_agent::fuzzing::dns_message(data));
