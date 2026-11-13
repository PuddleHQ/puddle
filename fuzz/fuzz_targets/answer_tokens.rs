// SPDX-License-Identifier: AGPL-3.0-or-later
//! An answer read for tokens Puddle does not hold, whole and in pieces: both must find the same kinds,
//! each once, and keep no more than one run between pieces. The check is `puddle_proxy::fuzzing::answer_tokens`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| puddle_proxy::fuzzing::answer_tokens(data));
