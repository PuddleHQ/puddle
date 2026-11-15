// SPDX-License-Identifier: AGPL-3.0-or-later
//! The DNS message the guest's stub reads, built from the wire grammar (header, labels, compression
//! pointers, records). The checks are in `dns_message_grammar` in `fuzz/checks/dns_message_grammar.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../checks/dns_message_grammar.rs"]
mod checks;
#[path = "../checks/dns_message.rs"]
mod dns_message;

fuzz_target!(|message: checks::Message| checks::dns_message_grammar(&message));
