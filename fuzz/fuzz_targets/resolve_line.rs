// SPDX-License-Identifier: GPL-3.0-or-later
//! The lookup lines the host reads from the guest agent, and the answer line read back. The checks
//! are in `puddle_agent_proto::fuzzing::resolve_line`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| puddle_agent_proto::fuzzing::resolve_line(data));
