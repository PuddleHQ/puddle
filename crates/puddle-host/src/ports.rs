// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of workspace ports: pinned forwards kept in the store, and the service the API
//! serves the Ports tab from ([`puddle_ports::Ports`] does the work).

use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_api::wire::{PortEntry, WorkspacePorts};
use puddle_api::{PortsService, PortsServiceError};
use puddle_ports::{Pin, Pins, PinsError, PortRow, Ports, PortsError};
use puddle_store::{PinnedPort, Store};
use puddle_types::WorkspaceName;

/// Pinned forwards in the database.
pub(crate) struct StorePins {
    store: Arc<Store>,
}

impl StorePins {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

fn pinned(pin: PinnedPort) -> Pin {
    Pin {
        guest_port: pin.guest_port,
        host_port: pin.host_port,
    }
}

fn failed(what: &str, err: &puddle_store::StoreError) -> PinsError {
    PinsError(format!("the pinned forwards could not be {what}: {err}"))
}

impl Pins for StorePins {
    fn pinned(&self, workspace: &WorkspaceName) -> Result<Vec<Pin>, PinsError> {
        self.store
            .pinned_ports(workspace)
            .map(|pins| pins.into_iter().map(pinned).collect())
            .map_err(|e| failed("read", &e))
    }

    fn all(&self) -> Result<Vec<(WorkspaceName, Pin)>, PinsError> {
        self.store
            .all_pinned_ports()
            .map(|pins| pins.into_iter().map(|(w, p)| (w, pinned(p))).collect())
            .map_err(|e| failed("read", &e))
    }

    fn pin(&self, workspace: &WorkspaceName, pin: Pin) -> Result<(), PinsError> {
        self.store
            .pin_port(
                workspace,
                PinnedPort {
                    guest_port: pin.guest_port,
                    host_port: pin.host_port,
                },
            )
            .map_err(|e| failed("saved", &e))
    }

    fn unpin(&self, workspace: &WorkspaceName, guest_port: u16) -> Result<bool, PinsError> {
        self.store
            .unpin_port(workspace, guest_port)
            .map_err(|e| failed("changed", &e))
    }
}

/// [`Ports`] as the API's service.
pub(crate) struct HostPorts {
    ports: Ports,
}

impl HostPorts {
    pub(crate) fn new(ports: Ports) -> Self {
        Self { ports }
    }
}

fn entry(row: PortRow) -> PortEntry {
    PortEntry {
        guest_port: row.guest_port,
        listening: row.listening,
        forwarded: row.forwarded,
        pinned: row.pinned,
        host_port: row.host_port,
        problem: row.problem,
    }
}

fn refusal(err: &PortsError) -> PortsServiceError {
    let text = err.to_string();
    match err {
        PortsError::NotRunning => PortsServiceError::NotRunning(text),
        PortsError::BadPort => PortsServiceError::Invalid(text),
        PortsError::Taken { .. } | PortsError::Cannot { .. } => PortsServiceError::Taken(text),
        PortsError::NotForwarded { .. } => PortsServiceError::NotForwarded(text),
        // Pins, and any refusal a later version adds: the user cannot fix it, the log has the text.
        _ => PortsServiceError::Failed(text),
    }
}

impl PortsService for HostPorts {
    fn list(
        &self,
        workspace: &WorkspaceName,
    ) -> BoxFuture<'_, Result<WorkspacePorts, PortsServiceError>> {
        let view = self.ports.view(workspace);
        Box::pin(async move {
            let view = view.map_err(|e| refusal(&e))?;
            Ok(WorkspacePorts {
                running: view.running,
                ports: view.ports.into_iter().map(entry).collect(),
            })
        })
    }

    fn forward(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
        host_port: Option<u16>,
        pinned: bool,
    ) -> BoxFuture<'_, Result<PortEntry, PortsServiceError>> {
        let workspace = workspace.clone();
        Box::pin(async move {
            self.ports
                .forward(&workspace, guest_port, host_port, pinned)
                .await
                .map(entry)
                .map_err(|e| refusal(&e))
        })
    }

    fn release(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
    ) -> BoxFuture<'_, Result<(), PortsServiceError>> {
        let workspace = workspace.clone();
        Box::pin(async move {
            self.ports
                .release(&workspace, guest_port)
                .await
                .map_err(|e| refusal(&e))
        })
    }
}
