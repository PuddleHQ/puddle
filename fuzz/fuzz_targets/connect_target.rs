// SPDX-License-Identifier: GPL-3.0-or-later
//! The target of a `CONNECT` or an absolute-form request, split from arbitrary text: the host
//! and port a guest names. The checks are in `puddle_proxy::fuzzing::target`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(uri) = std::str::from_utf8(data) {
        let _ = puddle_proxy::fuzzing::target("CONNECT", uri);
        let _ = puddle_proxy::fuzzing::target("GET", uri);
    }
});
