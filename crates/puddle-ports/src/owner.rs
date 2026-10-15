// SPDX-License-Identifier: GPL-3.0-or-later
//! Who has a port on this computer, for the message that says a forward could not use it.

use std::fmt;

use puddle_netpolicy::EndpointKind;
use puddle_types::WorkspaceName;

/// Whoever holds a port of this computer's loopback.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PortOwner {
    /// A forward of a workspace that runs now.
    Forward {
        /// The workspace.
        workspace: WorkspaceName,
        /// Its port that the forward leads to.
        guest_port: u16,
    },
    /// A forward pinned to a workspace that does not run now: it takes the port when it starts.
    Pinned {
        /// The workspace.
        workspace: WorkspaceName,
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
mod tests {
    use super::*;

    #[test]
    fn every_owner_reads_as_a_phrase_that_fits_after_in_use_by() {
        let ws = WorkspaceName::new("shop").unwrap();
        assert_eq!(
            PortOwner::Forward {
                workspace: ws.clone(),
                guest_port: 8080
            }
            .to_string(),
            "workspace shop's forward of its port 8080"
        );
        assert_eq!(
            PortOwner::Pinned {
                workspace: ws,
                guest_port: 3000
            }
            .to_string(),
            "workspace shop's pinned forward of its port 3000"
        );
        assert_eq!(
            PortOwner::Puddle(EndpointKind::Api).to_string(),
            "Puddle's own API"
        );
        assert_eq!(
            PortOwner::Program(Some("node".into())).to_string(),
            "another program (node)"
        );
        assert_eq!(
            PortOwner::Program(None).to_string(),
            "another program on this computer"
        );
    }

    #[test]
    fn the_system_names_a_program_that_listens_and_has_nothing_to_say_about_a_free_port() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let name = SystemPrograms.program(port);
        assert!(
            name.as_deref().is_some_and(|n| !n.is_empty()),
            "this process listens on {port}: {name:?}"
        );
        drop(listener);
        assert_eq!(SystemPrograms.program(port), None);
        assert_eq!(SystemPrograms.program(0), None);
    }
}
