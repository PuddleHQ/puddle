// SPDX-License-Identifier: AGPL-3.0-or-later
//! The DNS message the guest's stub reads, built from the wire grammar (header, labels, compression
//! pointers, records). The checks are in `dns_message_grammar` in `fuzz/checks/dns_message_grammar.rs`, the builder in
//! `fuzz/checks/dns_message_builder.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use puddle_fuzz::dns_message_builder::Message;
use puddle_fuzz::dns_message_grammar::dns_message_grammar;

fuzz_target!(|message: Message| dns_message_grammar(&message));
