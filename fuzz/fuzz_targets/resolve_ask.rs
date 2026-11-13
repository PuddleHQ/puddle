// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent's side of one lookup (`resolve::ask`) against a scripted host on an in-memory stream: replies
//! at, under and over the line limit, cut or empty replies, junk, a host that goes quiet. The checks are in
//! `puddle_agent_proto::fuzzing::resolve_ask`.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|exchange: puddle_agent_proto::fuzzing::AskExchange| {
    puddle_agent_proto::fuzzing::resolve_ask(&exchange);
});
