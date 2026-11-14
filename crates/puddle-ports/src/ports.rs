// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the host knows about each workspace's ports: which ones listen in the guest, which are
//! forwarded to this computer, which are pinned.
//!
//! A forward is a listener on `127.0.0.1` of this computer that leads to one port of a workspace
//! (see [`crate::forward`]). A **pinned** forward is kept in the store and comes back at every
//! start of the workspace; a plain one lives until the workspace stops or the user stops it.
//! Pinned or not, a forward never takes a port something else holds: it says who has it.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::GuestWatcher;
use puddle_netpolicy::{HostAddrs, OwnAddresses, PuddleEndpoints};
use puddle_types::{Event, EventSink, WorkspaceId};
use tokio::net::TcpListener;

use crate::forward::{Forward, LinkCell, MAX_CONNECTIONS, link_cell};
use crate::owner::{PortOwner, ProgramLookup, SystemPrograms};
use crate::pins::{Pin, Pins, PinsError};

/// Why a request about ports was refused. Every message is a whole sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PortsError {
    /// The workspace is not running, so there is nothing to forward to.
    #[error("the workspace is not running; start it first")]
    NotRunning,
    /// A port of 0.
    #[error("a port is a number from 1 to 65535")]
    BadPort,
    /// The port of this computer is spoken for.
    #[error("port {host_port} on this computer is already in use by {owner}; choose another port")]
    Taken {
        /// The port on this computer.
        host_port: u16,
        /// Who has it.
        owner: PortOwner,
    },
    /// Binding the port failed for a reason other than it being in use.
    #[error("port {host_port} on this computer cannot be opened: {reason}")]
    Cannot {
        /// The port on this computer.
        host_port: u16,
        /// The operating system's reason.
        reason: String,
    },
    /// Nothing is forwarded or pinned for that port of the workspace.
    #[error("port {guest_port} of the workspace is not forwarded or pinned")]
    NotForwarded {
        /// The port in the workspace.
        guest_port: u16,
    },
    /// The pins could not be read or written.
    #[error("{0}")]
    Pins(String),
}

impl From<PinsError> for PortsError {
    fn from(e: PinsError) -> Self {
        Self::Pins(e.0)
    }
}

/// One port of a workspace as the Ports tab shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortRow {
    /// The port in the workspace.
    pub guest_port: u16,
    /// A process in the workspace listens on it now.
    pub listening: bool,
    /// A forward to it is open now.
    pub forwarded: bool,
    /// It comes back at every start of the workspace.
    pub pinned: bool,
    /// The port on this computer: the open forward's, or the one a pinned forward wants or failed
    /// to take.
    pub host_port: Option<u16>,
    /// Why it does not work: the pinned forward could not be restored, or the last connection
    /// through it failed.
    pub problem: Option<String>,
}

/// What the Ports tab lists for one workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortsView {
    /// The workspace runs: ports can be forwarded.
    pub running: bool,
    /// Its ports, by guest port: what listens, what is forwarded, what is pinned.
    pub ports: Vec<PortRow>,
}

/// The row for `guest_port`, made empty when there is none yet.
fn at(rows: &mut BTreeMap<u16, PortRow>, guest_port: u16) -> &mut PortRow {
    rows.entry(guest_port).or_insert(PortRow {
        guest_port,
        listening: false,
        forwarded: false,
        pinned: false,
        host_port: None,
        problem: None,
    })
}

/// Why [`Ports::guest_link_within`] has no link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoLink {
    /// The workspace is stopped or was never started.
    NotRunning,
    /// The workspace runs, but its agent has not said hello yet.
    NotConnected,
}

/// A pinned forward that could not be put back at the start.
struct Failed {
    host_port: u16,
    reason: String,
}

#[derive(Default)]
struct Entry {
    running: bool,
    listening: Vec<u16>,
    link: Option<LinkCell>,
    forwards: BTreeMap<u16, Active>,
    failed: BTreeMap<u16, Failed>,
}

/// Which workspaces' agent sessions ended, from [`Ports::agent_sessions_ended`].
#[derive(Debug)]
pub struct EndedSessions(tokio::sync::broadcast::Receiver<WorkspaceId>);

impl EndedSessions {
    /// The workspace of the next agent session that ended. A receiver that fell behind skips
    /// to the oldest name still held; when the ports are gone it never returns.
    pub async fn next(&mut self) -> WorkspaceId {
        loop {
            match self.0.recv().await {
                Ok(workspace) => return workspace,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    std::future::pending::<()>().await;
                }
            }
        }
    }
}

struct Active {
    host_port: u16,
    forward: Forward,
}

struct Inner {
    endpoints: PuddleEndpoints,
    pins: Arc<dyn Pins>,
    events: Arc<dyn EventSink>,
    programs: Arc<dyn ProgramLookup>,
    host: Arc<dyn HostAddrs>,
    entries: Mutex<HashMap<WorkspaceId, Entry>>,
    /// Serialises the calls that change forwards, so two of them never race for one port.
    changing: tokio::sync::Mutex<()>,
    /// The workspaces whose agent session ended, for whoever wants to check on the VM.
    ended: tokio::sync::broadcast::Sender<WorkspaceId>,
}

/// The ports of every workspace. Cheap to clone; clones share everything.
#[derive(Clone)]
pub struct Ports {
    inner: Arc<Inner>,
    /// Connections one forward carries at once; set before the ports are shared.
    max_connections: usize,
}

impl std::fmt::Debug for Ports {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ports").finish_non_exhaustive()
    }
}

impl Ports {
    /// Ports that register their listeners in `endpoints`, keep pins in `pins` and tell the user
    /// through `events`.
    #[must_use]
    pub fn new(
        endpoints: PuddleEndpoints,
        pins: Arc<dyn Pins>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self::with_programs(endpoints, pins, events, Arc::new(SystemPrograms))
    }

    /// [`Ports::new`] with another way of naming the program that holds a port (tests).
    #[must_use]
    pub fn with_programs(
        endpoints: PuddleEndpoints,
        pins: Arc<dyn Pins>,
        events: Arc<dyn EventSink>,
        programs: Arc<dyn ProgramLookup>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                endpoints,
                pins,
                events,
                programs,
                host: Arc::new(OwnAddresses),
                entries: Mutex::new(HashMap::new()),
                changing: tokio::sync::Mutex::new(()),
                ended: tokio::sync::broadcast::channel(64).0,
            }),
            max_connections: MAX_CONNECTIONS,
        }
    }

    /// The same ports with a forward carrying at most `limit` connections at once.
    /// Call it before the ports are cloned: a clone made earlier keeps the old limit.
    #[must_use]
    pub const fn with_connection_limit(mut self, limit: usize) -> Self {
        self.max_connections = limit;
        self
    }

    /// Says which workspace's agent session ended, each time one does. A receiver that falls
    /// behind loses the oldest names; it is a hint to look, never the only way to find out.
    #[must_use]
    pub fn agent_sessions_ended(&self) -> EndedSessions {
        EndedSessions(self.inner.ended.subscribe())
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<WorkspaceId, Entry>> {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn changed(&self, workspace: &WorkspaceId) {
        self.inner.events.emit(Event::WorkspacePortsChanged {
            workspace: workspace.clone(),
        });
    }

    /// The workspace's pinned forwards are put back, and ports can be forwarded from now on. Call
    /// it once the workspace's boot has settled. A pinned forward whose port on this computer is
    /// taken is not opened; the user is told ([`Event::WorkspacePortForwardFailed`]) and the pin
    /// stays for the next start.
    pub async fn started(&self, workspace: &WorkspaceId) {
        let _changing = self.inner.changing.lock().await;
        self.entries().entry(workspace.clone()).or_default().running = true;
        match self.inner.pins.pinned(workspace) {
            Ok(pins) => {
                for pin in pins {
                    if let Err(err) = self.open(workspace, pin).await {
                        self.fail(workspace, pin, err.to_string());
                    }
                }
            }
            Err(err) => {
                tracing::warn!(%workspace, error = %err, "the pinned forwards could not be read");
                self.inner.events.emit(Event::WorkspacePortForwardFailed {
                    workspace: workspace.clone(),
                    guest_port: 0,
                    host_port: 0,
                    reason: format!("the pinned forwards could not be restored: {err}"),
                });
            }
        }
        self.changed(workspace);
    }

    fn fail(&self, workspace: &WorkspaceId, pin: Pin, reason: String) {
        tracing::warn!(%workspace, guest_port = pin.guest_port, host_port = pin.host_port, %reason, "a pinned forward could not be restored");
        if let Some(entry) = self.entries().get_mut(workspace) {
            entry.failed.insert(
                pin.guest_port,
                Failed {
                    host_port: pin.host_port,
                    reason: reason.clone(),
                },
            );
        }
        self.inner.events.emit(Event::WorkspacePortForwardFailed {
            workspace: workspace.clone(),
            guest_port: pin.guest_port,
            host_port: pin.host_port,
            reason,
        });
    }

    /// The way to open streams to `workspace`'s agent, `None` while the workspace is stopped or its
    /// agent has not said hello.
    #[must_use]
    pub fn guest_link(&self, workspace: &WorkspaceId) -> Option<GuestLink> {
        let entries = self.entries();
        entries.get(workspace)?.link.as_ref()?.borrow().clone()
    }

    /// [`Self::guest_link`], waiting up to `wait` for the agent's hello when the workspace is
    /// running and the agent has not said it yet (a start reports the workspace running once the
    /// agent listens, and its hello follows a moment later). It returns as soon as the link
    /// arrives, and at once when the workspace stops meanwhile.
    ///
    /// # Errors
    ///
    /// [`NoLink::NotRunning`] when the workspace is stopped or never started,
    /// [`NoLink::NotConnected`] when it runs and no agent said hello within `wait`.
    pub async fn guest_link_within(
        &self,
        workspace: &WorkspaceId,
        wait: Duration,
    ) -> Result<GuestLink, NoLink> {
        let mut link = {
            let mut entries = self.entries();
            let entry = entries.get_mut(workspace).ok_or(NoLink::NotRunning)?;
            entry.link.get_or_insert_with(link_cell).subscribe()
        };
        match tokio::time::timeout(wait, link.wait_for(Option::is_some)).await {
            Ok(Ok(link)) => link.clone().ok_or(NoLink::NotConnected),
            // Every sender is gone: `stopped` dropped the entry.
            Ok(Err(_)) => Err(NoLink::NotRunning),
            Err(_) => Err(NoLink::NotConnected),
        }
    }

    /// The workspace stopped: its forwards close and what the guest listened on is forgotten. Its
    /// pins stay. Idempotent.
    pub async fn stopped(&self, workspace: &WorkspaceId) {
        let _changing = self.inner.changing.lock().await;
        let gone = self.entries().remove(workspace);
        if let Some(entry) = gone {
            if let Some(link) = &entry.link {
                link.send_replace(None);
            }
            // The listeners are gone before this returns, and not closed under the lock.
            for active in entry.forwards.into_values() {
                active.forward.close().await;
            }
            self.changed(workspace);
        }
    }

    /// What the Ports tab shows for `workspace`.
    ///
    /// # Errors
    ///
    /// The pins could not be read.
    pub fn view(&self, workspace: &WorkspaceId) -> Result<PortsView, PortsError> {
        let pins = self.inner.pins.pinned(workspace)?;
        let entries = self.entries();
        let entry = entries.get(workspace);
        let mut rows: BTreeMap<u16, PortRow> = BTreeMap::new();
        for pin in pins {
            let row = at(&mut rows, pin.guest_port);
            row.pinned = true;
            row.host_port = Some(pin.host_port);
        }
        let running = entry.is_some_and(|e| e.running);
        if let Some(entry) = entry {
            for port in &entry.listening {
                at(&mut rows, *port).listening = true;
            }
            for (guest_port, failed) in &entry.failed {
                let row = at(&mut rows, *guest_port);
                row.host_port = Some(failed.host_port);
                row.problem = Some(failed.reason.clone());
            }
            for (guest_port, active) in &entry.forwards {
                let row = at(&mut rows, *guest_port);
                row.forwarded = true;
                row.host_port = Some(active.host_port);
                row.problem = active.forward.problem();
            }
        }
        Ok(PortsView {
            running,
            ports: rows.into_values().collect(),
        })
    }

    /// Forwards `guest_port` of the running workspace to `host_port` of this computer (the same
    /// number when `None`), and pins or unpins it as asked. Asking again for what is already so
    /// changes nothing; a different `host_port` moves the forward (the old one stays open until
    /// the new one is bound).
    ///
    /// # Errors
    ///
    /// [`PortsError`] says why not: the workspace does not run, the port is not a port, the port of
    /// this computer is in use (and by whom), or the pin could not be saved. A forward that was
    /// opened for a pin that could not be saved is closed again.
    pub async fn forward(
        &self,
        workspace: &WorkspaceId,
        guest_port: u16,
        host_port: Option<u16>,
        pinned: bool,
    ) -> Result<PortRow, PortsError> {
        let host_port = host_port.unwrap_or(guest_port);
        if guest_port == 0 || host_port == 0 {
            return Err(PortsError::BadPort);
        }
        let _changing = self.inner.changing.lock().await;
        if !self.entries().get(workspace).is_some_and(|e| e.running) {
            return Err(PortsError::NotRunning);
        }
        let pin = Pin {
            guest_port,
            host_port,
        };
        let was_pinned = self
            .inner
            .pins
            .pinned(workspace)?
            .iter()
            .any(|p| p.guest_port == guest_port);
        let opened = self.open(workspace, pin).await?;
        let saved = if pinned {
            self.inner.pins.pin(workspace, pin)
        } else if was_pinned {
            self.inner.pins.unpin(workspace, guest_port).map(drop)
        } else {
            Ok(())
        };
        if let Err(err) = saved {
            if opened {
                let undone = self
                    .entries()
                    .get_mut(workspace)
                    .and_then(|e| e.forwards.remove(&guest_port));
                if let Some(active) = undone {
                    active.forward.close().await;
                }
            }
            return Err(err.into());
        }
        self.changed(workspace);
        self.view(workspace)?
            .ports
            .into_iter()
            .find(|row| row.guest_port == guest_port)
            .ok_or(PortsError::NotForwarded { guest_port })
    }

    /// Stops the forward of `guest_port` and unpins it.
    ///
    /// # Errors
    ///
    /// [`PortsError::NotForwarded`] when there was neither a forward nor a pin, or the pins could
    /// not be changed.
    pub async fn release(
        &self,
        workspace: &WorkspaceId,
        guest_port: u16,
    ) -> Result<(), PortsError> {
        let _changing = self.inner.changing.lock().await;
        let (active, failed) = self.entries().get_mut(workspace).map_or((None, None), |e| {
            (e.forwards.remove(&guest_port), e.failed.remove(&guest_port))
        });
        let unpinned = self.inner.pins.unpin(workspace, guest_port)?;
        if active.is_none() && failed.is_none() && !unpinned {
            return Err(PortsError::NotForwarded { guest_port });
        }
        if let Some(active) = active {
            active.forward.close().await;
        }
        self.changed(workspace);
        Ok(())
    }

    /// Opens (or keeps, or moves) the forward for `pin`. `true` when a listener was made.
    async fn open(&self, workspace: &WorkspaceId, pin: Pin) -> Result<bool, PortsError> {
        let link = {
            let mut entries = self.entries();
            let entry = entries.entry(workspace.clone()).or_default();
            if entry.forwards.get(&pin.guest_port).map(|a| a.host_port) == Some(pin.host_port) {
                entry.failed.remove(&pin.guest_port);
                return Ok(false);
            }
            Arc::clone(entry.link.get_or_insert_with(link_cell))
        };
        if let Some(owner) = self.claimed(workspace, pin)? {
            return Err(PortsError::Taken {
                host_port: pin.host_port,
                owner,
            });
        }
        let listener = TcpListener::bind(("127.0.0.1", pin.host_port))
            .await
            .map_err(|err| self.bind_failed(pin.host_port, &err))?;
        let forward = Forward::start(
            listener,
            pin.guest_port,
            &link,
            &self.inner.endpoints,
            self.max_connections,
        )
        .map_err(|err| self.bind_failed(pin.host_port, &err))?;
        let old = {
            let mut entries = self.entries();
            let entry = entries.entry(workspace.clone()).or_default();
            entry.failed.remove(&pin.guest_port);
            entry.forwards.insert(
                pin.guest_port,
                Active {
                    host_port: pin.host_port,
                    forward,
                },
            )
        };
        if let Some(old) = old {
            old.forward.close().await;
        }
        Ok(true)
    }

    /// Who already holds `pin`'s port of this computer in Puddle's books: another forward, another
    /// pin, or one of Puddle's own listeners. Not the operating system's view: the bind says that.
    fn claimed(&self, workspace: &WorkspaceId, pin: Pin) -> Result<Option<PortOwner>, PortsError> {
        let holds =
            |w: &WorkspaceId, guest_port: u16| !(w == workspace && guest_port == pin.guest_port);
        for (name, entry) in self.entries().iter() {
            for (guest_port, active) in &entry.forwards {
                if active.host_port == pin.host_port && holds(name, *guest_port) {
                    return Ok(Some(PortOwner::Forward {
                        workspace: name.clone(),
                        guest_port: *guest_port,
                    }));
                }
            }
        }
        for (name, other) in self.inner.pins.all()? {
            if other.host_port == pin.host_port && holds(&name, other.guest_port) {
                return Ok(Some(PortOwner::Pinned {
                    workspace: name,
                    guest_port: other.guest_port,
                }));
            }
        }
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], pin.host_port));
        Ok(self
            .inner
            .endpoints
            .find(addr, self.inner.host.as_ref())
            .map(PortOwner::Puddle))
    }

    fn bind_failed(&self, host_port: u16, err: &std::io::Error) -> PortsError {
        bind_failure(host_port, err, self.inner.programs.as_ref())
    }
}

/// What a refused bind of `host_port` means to the user: the port is taken (and by which program),
/// or the system will not open it.
fn bind_failure(host_port: u16, err: &std::io::Error, programs: &dyn ProgramLookup) -> PortsError {
    if err.kind() == std::io::ErrorKind::AddrInUse {
        return PortsError::Taken {
            host_port,
            owner: PortOwner::Program(programs.program(host_port)),
        };
    }
    PortsError::Cannot {
        host_port,
        reason: if err.kind() == std::io::ErrorKind::PermissionDenied {
            "the system does not let Puddle use it (ports below 1024 and ranges the system reserves are closed to programs without special rights)".to_owned()
        } else {
            err.to_string()
        },
    }
}

impl GuestWatcher for Ports {
    fn link(&self, workspace: &WorkspaceId, link: GuestLink) {
        let mut entries = self.entries();
        let entry = entries.entry(workspace.clone()).or_default();
        entry
            .link
            .get_or_insert_with(link_cell)
            .send_replace(Some(link));
    }

    fn session_ended(&self, workspace: &WorkspaceId) {
        // No receiver is fine: nobody is watching.
        let _ = self.inner.ended.send(workspace.clone());
    }

    fn ports(&self, workspace: &WorkspaceId, ports: Vec<u16>) {
        let changed = {
            let mut entries = self.entries();
            let entry = entries.entry(workspace.clone()).or_default();
            if entry.listening == ports {
                false
            } else {
                entry.listening = ports;
                true
            }
        };
        if changed {
            self.changed(workspace);
        }
    }
}

#[cfg(test)]
mod tests;
