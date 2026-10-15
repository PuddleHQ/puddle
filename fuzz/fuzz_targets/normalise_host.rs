// SPDX-License-Identifier: GPL-3.0-or-later
//! Host normalisation: any text is refused or becomes a host that normalises to itself, in
//! printable ASCII. The checks are in `puddle_proxy::fuzzing::host`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| puddle_proxy::fuzzing::host(data));
