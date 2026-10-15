// SPDX-License-Identifier: GPL-3.0-or-later
//! The first line of a stream from the guest agent: accepted only as `\0puddle-<kind>/<version>\n`,
//! and a refusal never echoes control characters. The checks are in `puddle_proxy::fuzzing::preamble`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| puddle_proxy::fuzzing::preamble(data));
