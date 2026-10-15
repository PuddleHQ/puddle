// SPDX-License-Identifier: GPL-3.0-or-later
//! Where pinned forwards are kept. The host stores them in the database; the tests keep them in
//! memory.

use puddle_types::WorkspaceName;

/// A forward pinned to a workspace: it comes back, on the same port of this computer, every time
/// the workspace starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pin {
    /// The port in the workspace.
    pub guest_port: u16,
    /// The port on this computer.
    pub host_port: u16,
}

/// The store of pins failed. The text is for the user; it names what could not be done.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PinsError(pub String);

/// Where [`Pin`]s are kept.
pub trait Pins: Send + Sync + 'static {
    /// The pins of `workspace`, by guest port.
    ///
    /// # Errors
    ///
    /// The store could not be read.
    fn pinned(&self, workspace: &WorkspaceName) -> Result<Vec<Pin>, PinsError>;

    /// Every workspace's pins, to tell which ports of this computer are spoken for.
    ///
    /// # Errors
    ///
    /// The store could not be read.
    fn all(&self) -> Result<Vec<(WorkspaceName, Pin)>, PinsError>;

    /// Pins `pin` for `workspace`, replacing the pin of the same guest port.
    ///
    /// # Errors
    ///
    /// The store could not be written; nothing changed.
    fn pin(&self, workspace: &WorkspaceName, pin: Pin) -> Result<(), PinsError>;

    /// Removes the pin of `guest_port`; `true` when there was one.
    ///
    /// # Errors
    ///
    /// The store could not be written; nothing changed.
    fn unpin(&self, workspace: &WorkspaceName, guest_port: u16) -> Result<bool, PinsError>;
}
