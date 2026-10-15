// SPDX-License-Identifier: GPL-3.0-or-later
//! The ports the API serves: the [`PortsService`] trait the routes call, [`NoPorts`] for a build
//! without it, and [`FakePorts`] for tests and the UI fixture.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use futures_util::future::BoxFuture;
use puddle_types::WorkspaceName;

use crate::wire::{PortEntry, WorkspacePorts};

/// Why a request about ports was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PortsServiceError {
    /// Not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// The workspace is not running (409).
    #[error("{0}")]
    NotRunning(String),
    /// A port of this computer is in use, and by whom (409).
    #[error("{0}")]
    Taken(String),
    /// A value is out of range (422).
    #[error("{0}")]
    Invalid(String),
    /// Nothing is forwarded or pinned for that port (404).
    #[error("{0}")]
    NotForwarded(String),
    /// The pins could not be read or written (503, with what failed).
    #[error("{0}")]
    Failed(String),
}

/// What the Ports tab asks the host. Workspaces are named by their stored name.
pub trait PortsService: Send + Sync {
    /// The workspace's ports now.
    fn list(
        &self,
        workspace: &WorkspaceName,
    ) -> BoxFuture<'_, Result<WorkspacePorts, PortsServiceError>>;

    /// Forwards `guest_port` to `host_port` of this computer (the same number when `None`) and
    /// pins it or not; returns the port's row.
    fn forward(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
        host_port: Option<u16>,
        pinned: bool,
    ) -> BoxFuture<'_, Result<PortEntry, PortsServiceError>>;

    /// Stops the forward of `guest_port` and unpins it.
    fn release(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
    ) -> BoxFuture<'_, Result<(), PortsServiceError>>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPorts;

fn unavailable<T>() -> BoxFuture<'static, Result<T, PortsServiceError>> {
    Box::pin(async {
        Err(PortsServiceError::Unavailable(
            "ports are not available in this build".into(),
        ))
    })
}

impl PortsService for NoPorts {
    fn list(&self, _: &WorkspaceName) -> BoxFuture<'_, Result<WorkspacePorts, PortsServiceError>> {
        unavailable()
    }

    fn forward(
        &self,
        _: &WorkspaceName,
        _: u16,
        _: Option<u16>,
        _: bool,
    ) -> BoxFuture<'_, Result<PortEntry, PortsServiceError>> {
        unavailable()
    }

    fn release(&self, _: &WorkspaceName, _: u16) -> BoxFuture<'_, Result<(), PortsServiceError>> {
        unavailable()
    }
}

/// An in-memory service for tests and the UI fixture: every workspace runs, the ports it listens on
/// are what the test set, and a forward is a row. Nothing is bound.
#[derive(Debug, Default)]
pub struct FakePorts {
    state: Mutex<BTreeMap<WorkspaceName, BTreeMap<u16, PortEntry>>>,
}

impl FakePorts {
    /// A fake with nothing listening.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the ports `workspace` listens on (the rows for ports that are forwarded or pinned stay).
    pub fn set_listening(&self, workspace: &WorkspaceName, ports: &[u16]) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = state.entry(workspace.clone()).or_default();
        for row in rows.values_mut() {
            row.listening = false;
        }
        for port in ports {
            rows.entry(*port).or_insert_with(|| blank(*port)).listening = true;
        }
        rows.retain(|_, row| row.listening || row.forwarded || row.pinned);
    }
}

fn blank(guest_port: u16) -> PortEntry {
    PortEntry {
        guest_port,
        listening: false,
        forwarded: false,
        pinned: false,
        host_port: None,
        problem: None,
    }
}

impl PortsService for FakePorts {
    fn list(
        &self,
        workspace: &WorkspaceName,
    ) -> BoxFuture<'_, Result<WorkspacePorts, PortsServiceError>> {
        let ports = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .map(|rows| rows.values().cloned().collect())
            .unwrap_or_default();
        Box::pin(async move {
            Ok(WorkspacePorts {
                running: true,
                ports,
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
        let host_port = host_port.unwrap_or(guest_port);
        let result = if guest_port == 0 || host_port == 0 {
            Err(PortsServiceError::Invalid(
                "a port is a number from 1 to 65535".into(),
            ))
        } else {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            let taken = state.iter().find_map(|(name, rows)| {
                rows.values()
                    .find(|r| {
                        r.host_port == Some(host_port)
                            && (name != workspace || r.guest_port != guest_port)
                    })
                    .map(|r| (name.clone(), r.guest_port))
            });
            if let Some((name, port)) = taken {
                Err(PortsServiceError::Taken(format!(
                    "port {host_port} on this computer is already in use by workspace {name}'s forward of its port {port}; choose another port"
                )))
            } else {
                let row = state
                    .entry(workspace.clone())
                    .or_default()
                    .entry(guest_port)
                    .or_insert_with(|| blank(guest_port));
                row.forwarded = true;
                row.pinned = pinned;
                row.host_port = Some(host_port);
                Ok(row.clone())
            }
        };
        Box::pin(async move { result })
    }

    fn release(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
    ) -> BoxFuture<'_, Result<(), PortsServiceError>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = state.entry(workspace.clone()).or_default();
        let result = match rows.get_mut(&guest_port) {
            Some(row) if row.forwarded || row.pinned => {
                row.forwarded = false;
                row.pinned = false;
                row.host_port = None;
                if !row.listening {
                    rows.remove(&guest_port);
                }
                Ok(())
            }
            _ => Err(PortsServiceError::NotForwarded(format!(
                "port {guest_port} of the workspace is not forwarded or pinned"
            ))),
        };
        Box::pin(async move { result })
    }
}
