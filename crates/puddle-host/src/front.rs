// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of the browser editor's front: which guests it serves and with which server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_agent_proto::editor::{EditorServer, ServerKind};
use puddle_front::{FileHalves, Flavour, Front, PortsGuests};
use puddle_netpolicy::PuddleEndpoints;
use puddle_ports::Ports;
use puddle_types::WorkspaceName;

use crate::{HostError, HostPaths};

/// The server each running workspace was started with. The front serves what runs, not what the
/// settings say now: a consent withdrawn mid-run changes the next start, not the running server.
#[derive(Default)]
pub(crate) struct Flavours(Mutex<BTreeMap<WorkspaceName, Flavour>>);

impl Flavours {
    /// `name`'s guest runs with `editor` (`None`: the host bundles no code-server and the guest
    /// runs none, so there is nothing for the front to carry to).
    pub(crate) fn running(&self, name: &WorkspaceName, editor: Option<&EditorServer>) {
        let Some(editor) = editor else { return };
        let flavour = match editor.kind {
            ServerKind::Microsoft { .. } => Flavour::Microsoft,
            ServerKind::CodeServer => Flavour::CodeServer,
        };
        self.map().insert(name.clone(), flavour);
    }

    /// `name`'s guest does not run any more.
    pub(crate) fn stopped(&self, name: &WorkspaceName) {
        self.map().remove(name);
    }

    fn of(&self, name: &WorkspaceName) -> Flavour {
        self.map().get(name).copied().unwrap_or(Flavour::CodeServer)
    }

    fn map(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceName, Flavour>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
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
    let guests = PortsGuests::new(ports, move |workspace| flavours.of(workspace));
    Front::bind(
        None,
        endpoints,
        Arc::new(guests),
        Arc::new(FileHalves::new(paths.front_keys())),
    )
    .await
    .map_err(HostError::Front)
}
