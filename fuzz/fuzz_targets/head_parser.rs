// SPDX-License-Identifier: GPL-3.0-or-later
//! A guest's first bytes on a proxied stream: the request head, its target and the host's normal
//! form. The checks are in `puddle_proxy::fuzzing::head`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = puddle_proxy::fuzzing::head(data);
});
