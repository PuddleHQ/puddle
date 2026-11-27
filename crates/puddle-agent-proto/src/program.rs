// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name of the program that opened a connection, as the agent tells the proxy.
//!
//! The agent resolves the connecting socket to a process in the guest and sends only the process
//! name, as [`PROGRAM_HEADER`] on the `CONNECT`, so the Inbox can say which tool asked (`pip`,
//! `node`, `curl`). It never sends a command line, an environment, a directory, a user or a pid.
//!
//! The name is a **display hint**. A guest's root user can send any name, so it is never an input
//! to rule matching, a credential decision or a policy. [`Program`] (in `puddle-types`) keeps what survives
//! [`Program::sanitise`], which also removes every character a header could use to inject a line.

pub use puddle_types::{PROGRAM_MAX_CHARS, Program};

/// The request header that carries the name of the program that opened the connection.
pub const PROGRAM_HEADER: &str = "x-puddle-program";

#[cfg(test)]
mod tests;
