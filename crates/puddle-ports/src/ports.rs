// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the host knows about each workspace's ports: which ones listen in the guest, which are
//! forwarded to this computer, which are pinned.
//!
//! A forward is a listener on `127.0.0.1` of this computer that leads to one port of a workspace
//! (see [`crate::forward`]). A **pinned** forward is kept in the store and comes back at every
//! start of the workspace; a plain one lives until the workspace stops or the user stops it.
//! Pinned or not, a forward never takes a port something else holds: it says who has it.
//!
//! A port that starts listening in a running workspace is also forwarded on its own, to the same
//! number on this computer when that is free, and the forward closes again when the listener goes
//! (see [`Ports::ports`](GuestWatcher::ports)). That is what lets a tool in the workspace finish a
//! sign-in: it listens on a loopback port and the browser on this computer is sent back to it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::GuestWatcher;
use puddle_netpolicy::{HostAddrs, OwnAddresses, PuddleEndpoints};
use puddle_types::{Event, EventSink, WorkspaceId};

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
    /// The agent has reported its ports at least once. What it listens on at that first report was
    /// there before anyone asked, and is not forwarded on its own.
    reported: bool,
    /// Ports the user stopped the forward of while they listen: not forwarded again on their own
    /// until the listener goes.
    declined: BTreeSet<u16>,
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
    /// Opened by [`Ports::auto_forward`], not by the user: it closes when the listener goes.
    auto: bool,
}

/// What [`Ports::open`] did.
enum Opened {
    /// The forward was already on this port.
    Kept,
    /// A listener was made. A forward on another port that it moved is handed back still open, so
    /// a caller that cannot save the move can put it back.
    Bound { replaced: Option<Active> },
}

struct Inner {
    endpoints: PuddleEndpoints,
    pins: Arc<dyn Pins>,
    events: Arc<dyn EventSink>,
    programs: Arc<dyn ProgramLookup>,
    host: Arc<dyn HostAddrs>,
    entries: Mutex<HashMap<WorkspaceId, Entry>>,
    /// Workspaces stopped since they last started: a report their agent sends late is dropped,
    /// not turned into an entry again. Taken only while holding `entries`.
    stopped: Mutex<HashSet<WorkspaceId>>,
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
    /// New listeners are forwarded on their own ([`Self::with_auto_forward`]).
    auto_forward: bool,
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
                stopped: Mutex::new(HashSet::new()),
                changing: tokio::sync::Mutex::new(()),
                ended: tokio::sync::broadcast::channel(64).0,
            }),
            max_connections: MAX_CONNECTIONS,
            auto_forward: false,
        }
    }

    /// The same ports with a forward carrying at most `limit` connections at once.
    /// Call it before the ports are cloned: a clone made earlier keeps the old limit.
    #[must_use]
    pub const fn with_connection_limit(mut self, limit: usize) -> Self {
        self.max_connections = limit;
        self
    }

    /// The same ports, forwarding every port that starts listening in a running workspace on its
    /// own. Off by default so a test that reports ports never opens ports of the real computer.
    /// Call it before the ports are cloned.
    #[must_use]
    pub const fn with_auto_forward(mut self) -> Self {
        self.auto_forward = true;
        self
    }

    /// Says which workspace's agent session ended, each time one does. A receiver that falls
    /// behind loses the oldest names; it is a hint to look, never the only way to find out.
    #[must_use]
    pub fn agent_sessions_ended(&self) -> EndedSessions {
        EndedSessions(self.inner.ended.subscribe())
    }

    fn stopped_set(&self) -> MutexGuard<'_, HashSet<WorkspaceId>> {
        self.inner
            .stopped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
        {
            let mut entries = self.entries();
            self.stopped_set().remove(workspace);
            entries.entry(workspace.clone()).or_default().running = true;
        }
        match self.inner.pins.pinned(workspace) {
            Ok(pins) => {
                for pin in pins {
                    // Nothing runs yet, so no forward is moved here.
                    if let Err(err) = self.open(workspace, pin, false) {
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

    /// A boot of the workspace begins, before its agent can connect: the reports of this start are
    /// taken again (those of a session that outlived the last stop were dropped until now). The
    /// agent's hello comes during the boot, long before [`Self::started`].
    pub fn booting(&self, workspace: &WorkspaceId) {
        let entries = self.entries();
        self.stopped_set().remove(workspace);
        drop(entries);
    }

    /// The workspace stopped: its forwards close and what the guest listened on is forgotten. Its
    /// pins stay. What the workspace's agent reports after this is dropped until the next
    /// [`Self::started`]. Idempotent.
    pub async fn stopped(&self, workspace: &WorkspaceId) {
        let _changing = self.inner.changing.lock().await;
        let gone = {
            let mut entries = self.entries();
            self.stopped_set().insert(workspace.clone());
            entries.remove(workspace)
        };
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
        let opened = self.open(workspace, pin, false)?;
        let saved = if pinned {
            self.inner.pins.pin(workspace, pin)
        } else if was_pinned {
            self.inner.pins.unpin(workspace, guest_port).map(drop)
        } else {
            Ok(())
        };
        if let Err(err) = saved {
            if let Opened::Bound { replaced } = opened {
                // Put things back as they were: the new listener goes, the old forward serves again.
                let undone = {
                    let mut entries = self.entries();
                    entries.get_mut(workspace).and_then(|e| {
                        let undone = e.forwards.remove(&guest_port);
                        if let Some(old) = replaced {
                            e.forwards.insert(guest_port, old);
                        }
                        undone
                    })
                };
                if let Some(active) = undone {
                    active.forward.close().await;
                }
            }
            return Err(err.into());
        }
        if let Opened::Bound {
            replaced: Some(old),
        } = opened
        {
            old.forward.close().await;
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
        // Unpin first: when the pins cannot be changed the forward that works stays.
        let unpinned = self.inner.pins.unpin(workspace, guest_port)?;
        let (active, failed) = self.entries().get_mut(workspace).map_or((None, None), |e| {
            if e.listening.contains(&guest_port) {
                e.declined.insert(guest_port);
            }
            (e.forwards.remove(&guest_port), e.failed.remove(&guest_port))
        });
        if active.is_none() && failed.is_none() && !unpinned {
            return Err(PortsError::NotForwarded { guest_port });
        }
        if let Some(active) = active {
            active.forward.close().await;
        }
        self.changed(workspace);
        Ok(())
    }

    /// Opens (or keeps, or moves) the forward for `pin`; a forward it moved is not closed.
    fn open(&self, workspace: &WorkspaceId, pin: Pin, auto: bool) -> Result<Opened, PortsError> {
        let link = {
            let mut entries = self.entries();
            let entry = entries.entry(workspace.clone()).or_default();
            if let Some(active) = entry
                .forwards
                .get_mut(&pin.guest_port)
                .filter(|a| a.host_port == pin.host_port)
            {
                // The user asking for what an automatic forward already is makes it theirs.
                active.auto &= auto;
                entry.failed.remove(&pin.guest_port);
                return Ok(Opened::Kept);
            }
            Arc::clone(entry.link.get_or_insert_with(link_cell))
        };
        if let Some(owner) = self.claimed(workspace, pin)? {
            return Err(PortsError::Taken {
                host_port: pin.host_port,
                owner,
            });
        }
        let forward = Forward::start(
            pin.host_port,
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
                    auto,
                },
            )
        };
        Ok(Opened::Bound { replaced: old })
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
        if self.stopped_set().contains(workspace) {
            return;
        }
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
        let (changed, fresh, gone) = {
            let mut entries = self.entries();
            if self.stopped_set().contains(workspace) {
                return;
            }
            let entry = entries.entry(workspace.clone()).or_default();
            if entry.listening == ports && entry.reported {
                (false, Vec::new(), Vec::new())
            } else {
                let fresh: Vec<u16> = if entry.reported {
                    ports
                        .iter()
                        .copied()
                        .filter(|p| !entry.listening.contains(p))
                        .collect()
                } else {
                    Vec::new()
                };
                let gone: Vec<u16> = entry
                    .listening
                    .iter()
                    .copied()
                    .filter(|p| !ports.contains(p))
                    .collect();
                entry.declined.retain(|p| ports.contains(p));
                entry.reported = true;
                entry.listening = ports;
                (true, fresh, gone)
            }
        };
        if changed {
            self.changed(workspace);
        }
        if self.auto_forward && !(fresh.is_empty() && gone.is_empty()) {
            // The report arrives on the agent's control task; the forwards are opened and closed
            // beside it, so a slow bind never holds the next report up.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let ports = self.clone();
                let workspace = workspace.clone();
                runtime.spawn(async move { ports.auto_forward(&workspace, &fresh, &gone).await });
            }
        }
    }
}

impl Ports {
    /// Forwards the ports that began to listen, each to its own number on this computer, and closes
    /// the automatic forwards of the ports that stopped. A port of this computer that is taken is
    /// left alone without a word: the user did not ask for this forward, and the Ports tab still
    /// offers it.
    async fn auto_forward(&self, workspace: &WorkspaceId, fresh: &[u16], gone: &[u16]) {
        let _changing = self.inner.changing.lock().await;
        let mut closing = Vec::new();
        let mut opened = false;
        {
            let mut entries = self.entries();
            if let Some(entry) = entries.get_mut(workspace) {
                for port in gone {
                    if entry.forwards.get(port).is_some_and(|a| a.auto)
                        && let Some(active) = entry.forwards.remove(port)
                    {
                        closing.push(active);
                    }
                }
            }
        }
        let pinned = self.inner.pins.pinned(workspace).unwrap_or_default();
        for &port in fresh {
            let wanted = {
                let entries = self.entries();
                entries.get(workspace).is_some_and(|e| {
                    e.running
                        && e.listening.contains(&port)
                        && !e.declined.contains(&port)
                        && !e.forwards.contains_key(&port)
                }) && pinned.iter().all(|p| p.guest_port != port)
            };
            if !wanted {
                continue;
            }
            let pin = Pin {
                guest_port: port,
                host_port: port,
            };
            match self.open(workspace, pin, true) {
                Ok(_) => opened = true,
                Err(err) => {
                    tracing::debug!(%workspace, port, error = %err, "a port was not forwarded on its own");
                }
            }
        }
        let had_closed = !closing.is_empty();
        for active in closing {
            active.forward.close().await;
        }
        if opened || had_closed {
            self.changed(workspace);
        }
    }
}

#[cfg(test)]
mod tests;
