// SPDX-License-Identifier: AGPL-3.0-or-later
//! The lookup and answer lines of the agent protocol, built from a grammar (JSON spellings, key order,
//! escapes, unknown fields, broken variants). The checks are in
//! `puddle_agent_proto::fuzzing::resolve_line_grammar`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|line: puddle_agent_proto::fuzzing::LineGrammar| {
    puddle_agent_proto::fuzzing::resolve_line_grammar(&line);
});
