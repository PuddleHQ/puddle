// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of the browser editor's front: which guests it serves and with which server.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_agent_proto::editor::{EditorServer, ServerKind};
use puddle_front::{FileHalves, Flavour, Front, Guests, ServerHalves, Target};
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

    /// The workspaces that run Microsoft's server.
    pub(crate) fn on_microsoft(&self) -> Vec<WorkspaceName> {
        self.map()
            .iter()
            .filter(|(_, flavour)| **flavour == Flavour::Microsoft)
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub(crate) fn of(&self, name: &WorkspaceName) -> Option<Flavour> {
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
/// The port is the one the last run used, kept in [`HostPaths::front_port_file`]: a browser keeps
/// the editor's storage per origin, and the port is part of it. When that port is taken, the front
/// takes another and the new one is kept from then on.
///
/// The guests come back too (a window's sign-in asks them for the editor's password), and the
/// stored halves of Microsoft's secret key (a deleted workspace's is forgotten through them).
///
/// # Errors
///
/// [`HostError::Front`] when no loopback port can be bound.
pub(crate) async fn serve(
    paths: &HostPaths,
    endpoints: &PuddleEndpoints,
    ports: Ports,
    flavours: Arc<Flavours>,
) -> Result<(Front, Arc<dyn Guests>, Arc<dyn ServerHalves>), HostError> {
    let guests: Arc<dyn Guests> = Arc::new(HostGuests { ports, flavours });
    let file = paths.front_port_file();
    let usual = read_port(&file);
    let halves: Arc<dyn ServerHalves> = Arc::new(FileHalves::new(paths.front_keys()));
    let front = Front::bind(usual, endpoints, Arc::clone(&guests), Arc::clone(&halves))
        .await
        .map_err(HostError::Front)?;
    let port = front.local_addr().port();
    if usual != Some(port) {
        if front.preferred_port_was_taken() {
            tracing::warn!(
                port,
                "the browser editor's usual port is taken, so its address changes this run; \
                 a browser editor opened before starts with empty browser storage"
            );
        }
        // The front works without the file; the next run just takes another port.
        if let Err(err) = puddle_fs::private::write_atomic(&file, port.to_string().as_bytes()) {
            tracing::warn!(port, error = %err, "the browser editor's port could not be kept for the next run");
        }
    }
    Ok((front, guests, halves))
}

/// The port kept by an earlier run; a missing or unreadable file means there is none.
fn read_port(file: &Path) -> Option<u16> {
    let port: u16 = std::fs::read_to_string(file).ok()?.trim().parse().ok()?;
    (port != 0).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_workspaces_started_on_microsoft_s_server_are_listed_and_a_stop_removes_one() {
        let flavours = Flavours::default();
        let name = |s: &str| WorkspaceName::new(s).unwrap();
        let microsoft = EditorServer::microsoft("/data", "/mnt", 300, false, true);
        flavours.running(&name("ms"), Some(&microsoft));
        flavours.running(&name("cs"), Some(&EditorServer::new("/data", "/mnt", 300)));
        flavours.running(&name("none"), None);
        assert_eq!(flavours.on_microsoft(), [name("ms")]);
        flavours.stopped(&name("ms"));
        assert_eq!(flavours.on_microsoft().len(), 0);
    }
}
