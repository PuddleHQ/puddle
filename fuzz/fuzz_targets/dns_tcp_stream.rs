// SPDX-License-Identifier: AGPL-3.0-or-later
//! A TCP byte stream to the stub: length-prefixed messages with honest and lying prefixes, the limit and
//! one past it, a stream cut anywhere. The checks are in `fuzz/checks/dns_transport.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use puddle_fuzz::dns_transport as checks;

fuzz_target!(|stream: checks::Stream| checks::dns_tcp_stream(&stream));
