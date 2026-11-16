// SPDX-License-Identifier: AGPL-3.0-or-later
//! One UDP datagram to the stub's socket: a grammar-built message, a query whose `OPT` record ends at the receive
//! buffer's limit, or raw bytes. The checks are in `fuzz/checks/dns_transport.rs`.
#![no_main]

use libfuzzer_sys::fuzz_target;
use puddle_fuzz::dns_transport as checks;

fuzz_target!(|probe: checks::Probe| checks::dns_udp_datagram(&probe));
