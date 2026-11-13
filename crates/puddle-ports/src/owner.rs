// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who has a port on this computer, for the message that says a forward could not use it.

use std::fmt;

use puddle_netpolicy::EndpointKind;
use puddle_types::WorkspaceId;

/// Whoever holds a port of this computer's loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PortOwner {
    /// A forward of a workspace that runs now.
    Forward {
        /// The workspace.
        workspace: WorkspaceId,
        /// Its port that the forward leads to.
        guest_port: u16,
    },
    /// A forward pinned to a workspace that does not run now: it takes the port when it starts.
    Pinned {
        /// The workspace.
        workspace: WorkspaceId,
        /// Its port that the pin leads to.
        guest_port: u16,
    },
    /// One of Puddle's own listeners.
    Puddle(EndpointKind),
    /// Another program on this computer; its name when the system tells it.
    Program(Option<String>),
}

impl fmt::Display for PortOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forward {
                workspace,
                guest_port,
            } => write!(
                f,
                "workspace {workspace}'s forward of its port {guest_port}"
            ),
            Self::Pinned {
                workspace,
                guest_port,
            } => write!(
                f,
                "workspace {workspace}'s pinned forward of its port {guest_port}"
            ),
            Self::Puddle(kind) => write!(f, "Puddle's own {kind}"),
            Self::Program(Some(name)) => write!(f, "another program ({name})"),
            Self::Program(None) => f.write_str("another program on this computer"),
        }
    }
}

/// Finds the program that listens on a port. A seam: the tests name their own.
pub trait ProgramLookup: Send + Sync + 'static {
    /// The name of the program that listens on TCP `port`, when the system says.
    fn program(&self, port: u16) -> Option<String>;
}

/// The operating system's answer (the `listeners` crate: `/proc` on Linux, the IP helper tables
/// on Windows, `libproc` on macOS). A program of another user may not be visible: `None` then.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemPrograms;

impl ProgramLookup for SystemPrograms {
    fn program(&self, port: u16) -> Option<String> {
        let process = listeners::get_process_by_port(port, listeners::Protocol::TCP).ok()?;
        let name: String = process
            .name
            .chars()
            .filter(|c| !c.is_control())
            .take(64)
            .collect();
        (!name.is_empty()).then_some(name)
    }
}

#[cfg(test)]
mod tests;
