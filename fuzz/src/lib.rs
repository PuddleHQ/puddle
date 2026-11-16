// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks and builders the cargo-fuzz targets share, as library modules so that each item is
//! public and a target that needs only some of them does not carry the rest as dead code. The
//! sources live in `fuzz/checks/`; `puddle-agent`'s `fuzz_corpus` test includes this file to replay
//! the checked-in corpora on the stable toolchain.

#[path = "../checks/dns_message.rs"]
pub mod dns_message;
#[path = "../checks/dns_message_builder.rs"]
pub mod dns_message_builder;
#[path = "../checks/dns_message_grammar.rs"]
pub mod dns_message_grammar;
#[path = "../checks/dns_transport.rs"]
pub mod dns_transport;
