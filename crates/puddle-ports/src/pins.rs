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

/// Pins kept in memory: for tests and for a host without a database.
#[derive(Debug, Default)]
pub struct MemoryPins {
    rows: std::sync::Mutex<Vec<(WorkspaceName, Pin)>>,
}

impl MemoryPins {
    fn rows(&self) -> std::sync::MutexGuard<'_, Vec<(WorkspaceName, Pin)>> {
        self.rows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Pins for MemoryPins {
    fn pinned(&self, workspace: &WorkspaceName) -> Result<Vec<Pin>, PinsError> {
        let mut pins: Vec<Pin> = self
            .rows()
            .iter()
            .filter(|(w, _)| w == workspace)
            .map(|(_, p)| *p)
            .collect();
        pins.sort();
        Ok(pins)
    }

    fn all(&self) -> Result<Vec<(WorkspaceName, Pin)>, PinsError> {
        Ok(self.rows().clone())
    }

    fn pin(&self, workspace: &WorkspaceName, pin: Pin) -> Result<(), PinsError> {
        let mut rows = self.rows();
        rows.retain(|(w, p)| !(w == workspace && p.guest_port == pin.guest_port));
        rows.push((workspace.clone(), pin));
        Ok(())
    }

    fn unpin(&self, workspace: &WorkspaceName, guest_port: u16) -> Result<bool, PinsError> {
        let mut rows = self.rows();
        let before = rows.len();
        rows.retain(|(w, p)| !(w == workspace && p.guest_port == guest_port));
        Ok(rows.len() < before)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(guest_port: u16, host_port: u16) -> Pin {
        Pin {
            guest_port,
            host_port,
        }
    }

    #[test]
    fn memory_pins_keep_replace_list_and_remove_per_workspace() {
        let pins = MemoryPins::default();
        let (a, b) = (
            WorkspaceName::new("a").unwrap(),
            WorkspaceName::new("b").unwrap(),
        );
        pins.pin(&a, pin(80, 8080)).unwrap();
        pins.pin(&a, pin(22, 2222)).unwrap();
        pins.pin(&b, pin(80, 8081)).unwrap();
        pins.pin(&a, pin(80, 9090)).unwrap();
        assert_eq!(pins.pinned(&a).unwrap(), [pin(22, 2222), pin(80, 9090)]);
        assert_eq!(pins.all().unwrap().len(), 3);
        assert!(pins.unpin(&a, 80).unwrap());
        assert!(!pins.unpin(&a, 80).unwrap());
        assert_eq!(pins.pinned(&b).unwrap(), [pin(80, 8081)]);
    }
}
