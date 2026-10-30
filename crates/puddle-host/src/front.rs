// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of the browser editor's front: which guests it serves and with which server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_agent_proto::editor::{EditorServer, ServerKind};
use puddle_front::{FileHalves, Flavour, Front, Guests, Target};
use puddle_netpolicy::PuddleEndpoints;
use puddle_ports::Ports;
use puddle_types::WorkspaceName;

use crate::{HostError, HostPaths};

/// The server each running workspace was started with. The front serves what runs, not what the
/// settings say now: a consent withdrawn mid-run changes the next start, not the running server.
#[derive(Default)]
pub(crate) struct Flavours(Mutex<BTreeMap<WorkspaceName, Flavour>>);

impl Flavours {
    /// `name`'s guest runs with `editor`. `None` means the host bundles no code-server, so the
    /// guest runs none: the front still reaches the agent and says the editor is not there.
    pub(crate) fn running(&self, name: &WorkspaceName, editor: Option<&EditorServer>) {
        let flavour = match editor.map(|e| e.kind) {
            Some(ServerKind::Microsoft { .. }) => Flavour::Microsoft,
            Some(ServerKind::CodeServer) | None => Flavour::CodeServer,
        };
        self.map().insert(name.clone(), flavour);
    }

    /// `name`'s guest does not run any more.
    pub(crate) fn stopped(&self, name: &WorkspaceName) {
        self.map().remove(name);
    }

    fn of(&self, name: &WorkspaceName) -> Option<Flavour> {
        self.map().get(name).copied()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceName, Flavour>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The workspaces the front serves: those with a connected agent whose server is known. A guest
/// whose start has not recorded its server yet is not served, rather than served with the wrong
/// flavour.
struct HostGuests {
    ports: Ports,
    flavours: Arc<Flavours>,
}

impl Guests for HostGuests {
    fn target(&self, workspace: &WorkspaceName) -> Option<Target> {
        let flavour = self.flavours.of(workspace)?;
        let link = self.ports.guest_link(workspace)?;
        Some(Target {
            editor: Arc::new(link),
            flavour,
        })
    }
}

/// Starts the front over the workspaces `ports` holds an agent link for, entered in `endpoints`
/// as one of Puddle's own listeners.
///
/// # Errors
///
/// [`HostError::Front`] when no loopback port can be bound.
pub(crate) async fn serve(
    paths: &HostPaths,
    endpoints: &PuddleEndpoints,
    ports: Ports,
    flavours: Arc<Flavours>,
) -> Result<Front, HostError> {
    let guests = HostGuests { ports, flavours };
    Front::bind(
        None,
        endpoints,
        Arc::new(guests),
        Arc::new(FileHalves::new(paths.front_keys())),
    )
    .await
    .map_err(HostError::Front)
}
