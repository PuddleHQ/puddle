// SPDX-License-Identifier: AGPL-3.0-or-later
//! A TCP byte stream to the stub: length-prefixed messages with honest and lying prefixes, the limit and
//! one past it, a stream cut anywhere. The checks are in `fuzz/checks/dns_transport.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;

#[path = "../checks/dns_transport.rs"]
mod checks;
#[path = "../checks/dns_message.rs"]
#[allow(dead_code, reason = "only the grammar module calls it here")]
mod dns_message;
#[path = "../checks/dns_message_grammar.rs"]
#[allow(dead_code, reason = "this target needs the message builder, not the grammar's checks")]
mod dns_message_grammar;

fuzz_target!(|stream: checks::Stream| checks::dns_tcp_stream(&stream));
