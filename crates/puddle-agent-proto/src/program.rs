// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name of the program that opened a connection, as the agent tells the proxy.
//!
//! The agent resolves the connecting socket to a process in the guest and sends only the process
//! name, as [`PROGRAM_HEADER`] on the `CONNECT`, so the Inbox can say which tool asked (`pip`,
//! `node`, `curl`). It never sends a command line, an environment, a directory, a user or a pid.
//!
//! The name is a **display hint**. A guest's root user can send any name, so it is never an input
//! to rule matching, a credential decision or a policy. [`Program`] keeps what survives
//! [`Program::sanitise`], which also removes every character a header could use to inject a line.

use std::fmt;

/// The request header that carries the name of the program that opened the connection.
pub const PROGRAM_HEADER: &str = "x-puddle-program";

/// The most characters a [`Program`] holds.
pub const PROGRAM_MAX_CHARS: usize = 32;

/// A process name that is safe to put in a header, a log line and the UI: 1 to
/// [`PROGRAM_MAX_CHARS`] characters of `A-Z a-z 0-9 . _ + -`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Program(String);

impl Program {
    /// Cleans `raw` by dropping every character outside `A-Z a-z 0-9 . _ + -` and keeping the
    /// first [`PROGRAM_MAX_CHARS`] of the rest. `None` when nothing is left.
    #[must_use]
    pub fn sanitise(raw: &str) -> Option<Self> {
        let name: String = raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
            .take(PROGRAM_MAX_CHARS)
            .collect();
        (!name.is_empty()).then_some(Self(name))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
