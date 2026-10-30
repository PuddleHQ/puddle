// SPDX-License-Identifier: GPL-3.0-or-later
//! The ports of a workspace, end to end: a real agent, the host's session code, `Ports` as the
//! watcher, and a "guest" that is this machine's loopback. What the user sees is the view and the
//! events; what a program sees is a listener on `127.0.0.1` that carries bytes.
#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_agent::config::{PortWatch, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::{
    GuestStream, GuestWatcher, HostConfig, StreamHandler, serve_session,
};
use puddle_ipc::IpcRoot;
use puddle_netpolicy::{EndpointKind, OwnAddresses, PuddleEndpoints};
use puddle_ports::{Pin, Pins, PinsError, PortOwner, PortRow, Ports, PortsError, ProgramLookup};
use puddle_types::{CollectingSink, Event, WorkspaceName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn name(text: &str) -> WorkspaceName {
    WorkspaceName::new(text).unwrap()
}

/// Pins in memory; `broken` makes every call fail.
#[derive(Default)]
struct MemoryPins {
    rows: Mutex<Vec<(WorkspaceName, Pin)>>,
    /// Every call fails.
    broken: Mutex<bool>,
    /// Reads work, writes fail.
    read_only: Mutex<bool>,
}

impl MemoryPins {
    fn check(&self) -> Result<(), PinsError> {
        if *self.broken.lock().unwrap() {
            Err(PinsError("the database is locked".into()))
        } else {
            Ok(())
        }
    }
}

impl MemoryPins {
    fn writable(&self) -> Result<(), PinsError> {
        if *self.read_only.lock().unwrap() {
            Err(PinsError("the database is read-only".into()))
        } else {
            Ok(())
        }
    }
}

impl Pins for MemoryPins {
    fn pinned(&self, workspace: &WorkspaceName) -> Result<Vec<Pin>, PinsError> {
        self.check()?;
        let mut pins: Vec<Pin> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(w, _)| w == workspace)
            .map(|(_, p)| *p)
            .collect();
        pins.sort();
        Ok(pins)
    }

    fn all(&self) -> Result<Vec<(WorkspaceName, Pin)>, PinsError> {
        self.check()?;
        Ok(self.rows.lock().unwrap().clone())
    }

    fn pin(&self, workspace: &WorkspaceName, pin: Pin) -> Result<(), PinsError> {
        self.check()?;
        self.writable()?;
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|(w, p)| !(w == workspace && p.guest_port == pin.guest_port));
        rows.push((workspace.clone(), pin));
        Ok(())
    }

    fn unpin(&self, workspace: &WorkspaceName, guest_port: u16) -> Result<bool, PinsError> {
        self.check()?;
        self.writable()?;
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|(w, p)| !(w == workspace && p.guest_port == guest_port));
        Ok(rows.len() < before)
    }
}

struct Named(Option<&'static str>);

impl ProgramLookup for Named {
    fn program(&self, _port: u16) -> Option<String> {
        self.0.map(str::to_owned)
    }
}

/// The route's end: feeds the session's reports to `Ports`, as the proxy's handler does.
#[derive(Clone)]
struct Route {
    ports: Ports,
    workspace: WorkspaceName,
}

impl StreamHandler for Route {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        self.ports.link(&self.workspace, link);
    }

    async fn guest_ports(&self, ports: Vec<u16>) {
        self.ports.ports(&self.workspace, ports);
    }
}

struct Rig {
    ports: Ports,
    pins: Arc<MemoryPins>,
    events: Arc<CollectingSink>,
    endpoints: PuddleEndpoints,
    workspace: WorkspaceName,
    socket: PathBuf,
    tables: PathBuf,
    _root: IpcRoot,
    agent: Option<Agent>,
    _dir: tempfile::TempDir,
}

async fn rig(tag: &str) -> Rig {
    rig_with(tag, |ports| ports).await
}

async fn rig_with(tag: &str, tweak: impl FnOnce(Ports) -> Ports) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix(&format!("pp-{tag}-"))
        .tempdir()
        .unwrap();
    let tables = dir.path().join("net");
    std::fs::create_dir_all(&tables).unwrap();
    let pins = Arc::new(MemoryPins::default());
    let events = Arc::new(CollectingSink::default());
    let endpoints = PuddleEndpoints::new();
    let ports = tweak(Ports::with_programs(
        endpoints.clone(),
        pins.clone(),
        events.clone(),
        Arc::new(Named(Some("someserver"))),
    ));
    let workspace = name("shop");
    let root = IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let socket = listener.endpoint().path().to_path_buf();
    let route = Route {
        ports: ports.clone(),
        workspace: workspace.clone(),
    };
    tokio::spawn(async move {
        loop {
            let conn = listener.accept().await.unwrap();
            let route = route.clone();
            tokio::spawn(async move {
                let sink = Arc::new(CollectingSink::default());
                let _ = serve_session(
                    conn,
                    route.workspace.clone(),
                    sink,
                    Arc::new(route),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
    let mut rig = Rig {
        ports,
        pins,
        events,
        endpoints,
        workspace,
        socket,
        tables,
        _root: root,
        agent: None,
        _dir: dir,
    };
    rig.start_agent().await;
    rig
}

impl Rig {
    async fn start_agent(&mut self) {
        let config = Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            target: Target::Unix(self.socket.clone()),
            oom: None,
            ports: Some(PortWatch {
                dir: self.tables.clone(),
                poll: Duration::from_millis(20),
            }),
            ..Config::default()
        };
        self.agent = Some(Agent::start(config).await.unwrap());
    }

    /// The guest's socket table with a wildcard listener on each of `ports`.
    fn guest_listens(&self, ports: &[u16]) {
        let mut rows = String::new();
        for p in ports {
            writeln!(
                rows,
                "   0: 00000000:{p:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0"
            )
            .unwrap();
        }
        std::fs::write(
            self.tables.join("tcp"),
            format!("  sl  local_address rem_address   st\n{rows}"),
        )
        .unwrap();
    }

    /// Waits until the host's view has a row for `guest_port` that `ok` accepts.
    async fn row(&self, guest_port: u16, ok: impl Fn(&PortRow) -> bool) -> PortRow {
        for _ in 0..500 {
            let view = self.ports.view(&self.workspace).unwrap();
            if let Some(row) = view
                .ports
                .into_iter()
                .find(|r| r.guest_port == guest_port && ok(r))
            {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "no such row for {guest_port}: {:?}",
            self.ports.view(&self.workspace)
        );
    }

    /// Takes the events so far.
    fn events(&self) -> Vec<Event> {
        self.events.take()
    }

    async fn started(&self) {
        self.ports.started(&self.workspace).await;
    }
}

/// A free port on this computer's loopback, held by nobody when it returns.
async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

/// An echo server on the "guest's" loopback; the port it listens on.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    port
}

async fn through(host_port: u16, bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut client = TcpStream::connect(("127.0.0.1", host_port)).await?;
    client.write_all(bytes).await?;
    client.shutdown().await?;
    let mut back = Vec::new();
    client.read_to_end(&mut back).await?;
    Ok(back)
}

#[tokio::test]
async fn what_the_guest_listens_on_is_listed_and_a_change_is_announced() {
    let rig = rig("listen").await;
    rig.started().await;
    rig.events();
    rig.guest_listens(&[8080, 3000]);
    rig.row(3000, |r| r.listening).await;
    let view = rig.ports.view(&rig.workspace).unwrap();
    assert!(view.running);
    assert_eq!(
        view.ports
            .iter()
            .map(|r| (r.guest_port, r.listening, r.forwarded))
            .collect::<Vec<_>>(),
        [(3000, true, false), (8080, true, false)]
    );
    assert!(rig.events().contains(&Event::WorkspacePortsChanged {
        workspace: rig.workspace.clone()
    }));
    // A port that stops listening drops out of the list.
    rig.guest_listens(&[8080]);
    for _ in 0..500 {
        if rig.ports.view(&rig.workspace).unwrap().ports.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        rig.ports.view(&rig.workspace).unwrap().ports[0].guest_port,
        8080
    );
}

#[tokio::test]
async fn a_forward_carries_bytes_to_the_guest_and_is_one_of_puddles_own_listeners() {
    let rig = rig("carry").await;
    rig.started().await;
    let guest = echo().await;
    let host = free_port().await;
    let row = rig
        .ports
        .forward(&rig.workspace, guest, Some(host), false)
        .await
        .unwrap();
    assert_eq!(
        row,
        PortRow {
            guest_port: guest,
            listening: false,
            forwarded: true,
            pinned: false,
            host_port: Some(host),
            problem: None
        }
    );
    assert_eq!(
        through(host, b"hello through").await.unwrap(),
        b"hello through"
    );
    // The guard's registry knows it, so no workspace can reach it from inside.
    let addr = SocketAddr::from(([127, 0, 0, 1], host));
    assert_eq!(
        rig.endpoints.find(addr, &OwnAddresses),
        Some(EndpointKind::PinnedForward)
    );
    // Asking again for what is so changes nothing, and the same number is the default host port.
    rig.ports
        .forward(&rig.workspace, guest, Some(host), false)
        .await
        .unwrap();
    assert_eq!(through(host, b"again").await.unwrap(), b"again");
    // Stopping closes the listener and takes it out of the registry.
    rig.ports.release(&rig.workspace, guest).await.unwrap();
    assert!(TcpStream::connect(addr).await.is_err());
    assert_eq!(rig.endpoints.find(addr, &OwnAddresses), None);
    assert_eq!(
        rig.ports.release(&rig.workspace, guest).await,
        Err(PortsError::NotForwarded { guest_port: guest })
    );
}

#[tokio::test]
async fn a_forward_defaults_to_the_same_port_number_and_moves_when_asked_for_another() {
    let rig = rig("move").await;
    rig.started().await;
    let guest = echo().await;
    let (first, second) = (free_port().await, free_port().await);
    rig.ports
        .forward(&rig.workspace, guest, Some(first), true)
        .await
        .unwrap();
    let moved = rig
        .ports
        .forward(&rig.workspace, guest, Some(second), true)
        .await
        .unwrap();
    assert_eq!(moved.host_port, Some(second));
    assert!(
        TcpStream::connect(("127.0.0.1", first)).await.is_err(),
        "the old port is closed"
    );
    assert_eq!(through(second, b"moved").await.unwrap(), b"moved");
    assert_eq!(
        rig.pins.pinned(&rig.workspace).unwrap(),
        [Pin {
            guest_port: guest,
            host_port: second
        }]
    );
    // No host port named: the guest's own number. It is free here only by luck, so use a guest
    // port that is also free on this side by forwarding to it from a fresh rig port.
    let same = free_port().await;
    // The echo server above is not on `same`, but a forward to a port nothing listens on is
    // still opened: connections to it fail and the row says why.
    let row = rig
        .ports
        .forward(&rig.workspace, same, None, false)
        .await
        .unwrap();
    assert_eq!(row.host_port, Some(same));
}

#[tokio::test]
async fn a_pinned_forward_survives_a_stop_and_comes_back_at_the_next_start() {
    let rig = rig("pinned").await;
    rig.started().await;
    let guest = echo().await;
    let host = free_port().await;
    rig.ports
        .forward(&rig.workspace, guest, Some(host), true)
        .await
        .unwrap();
    rig.ports.stopped(&rig.workspace).await;
    assert!(TcpStream::connect(("127.0.0.1", host)).await.is_err());
    // Stopped: the pin is listed, nothing runs.
    let view = rig.ports.view(&rig.workspace).unwrap();
    assert!(!view.running);
    assert_eq!(
        view.ports,
        [PortRow {
            guest_port: guest,
            listening: false,
            forwarded: false,
            pinned: true,
            host_port: Some(host),
            problem: None
        }]
    );
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest, Some(host), true)
            .await,
        Err(PortsError::NotRunning)
    );
    // The agent connects again as it does after a start, and the pin is put back.
    rig.started().await;
    let row = rig.row(guest, |r| r.forwarded).await;
    assert!(row.pinned && row.host_port == Some(host));
    assert_eq!(through(host, b"restored").await.unwrap(), b"restored");
}

#[tokio::test]
async fn a_port_of_this_computer_that_is_taken_is_refused_naming_who_has_it() {
    let rig = rig("taken").await;
    rig.started().await;
    let guest = echo().await;
    // Another program.
    let program = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = program.local_addr().unwrap().port();
    let err = rig
        .ports
        .forward(&rig.workspace, guest, Some(port), false)
        .await
        .unwrap_err();
    assert_eq!(
        err,
        PortsError::Taken {
            host_port: port,
            owner: PortOwner::Program(Some("someserver".into()))
        }
    );
    assert_eq!(
        err.to_string(),
        format!(
            "port {port} on this computer is already in use by another program (someserver); choose another port"
        )
    );
    // Puddle's own listener.
    let own = free_port().await;
    let _api = rig
        .endpoints
        .register(SocketAddr::from(([127, 0, 0, 1], own)), EndpointKind::Api);
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest, Some(own), false)
            .await
            .unwrap_err(),
        PortsError::Taken {
            host_port: own,
            owner: PortOwner::Puddle(EndpointKind::Api)
        }
    );
    // Another forward of this or another workspace's.
    let host = free_port().await;
    rig.ports
        .forward(&rig.workspace, guest, Some(host), false)
        .await
        .unwrap();
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest + 1, Some(host), false)
            .await
            .unwrap_err(),
        PortsError::Taken {
            host_port: host,
            owner: PortOwner::Forward {
                workspace: rig.workspace.clone(),
                guest_port: guest
            }
        }
    );
    // A pin of a workspace that does not run keeps its port.
    let pinned = free_port().await;
    rig.pins
        .pin(
            &name("blog"),
            Pin {
                guest_port: 80,
                host_port: pinned,
            },
        )
        .unwrap();
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest, Some(pinned), false)
            .await
            .unwrap_err(),
        PortsError::Taken {
            host_port: pinned,
            owner: PortOwner::Pinned {
                workspace: name("blog"),
                guest_port: 80
            }
        }
    );
    // Nothing was pinned or opened by the refused calls.
    assert_eq!(rig.pins.pinned(&rig.workspace).unwrap(), []);
    // A program the system does not name is still "another program".
    drop(program);
}

#[tokio::test]
async fn a_pinned_forward_that_cannot_be_restored_is_announced_listed_and_kept() {
    let rig = rig("failed").await;
    let busy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = busy.local_addr().unwrap().port();
    rig.pins
        .pin(
            &rig.workspace,
            Pin {
                guest_port: 8080,
                host_port: port,
            },
        )
        .unwrap();
    rig.started().await;
    let events = rig.events();
    let failed = events
        .iter()
        .find_map(|e| match e {
            Event::WorkspacePortForwardFailed {
                guest_port,
                host_port,
                reason,
                ..
            } => Some((*guest_port, *host_port, reason.clone())),
            _ => None,
        })
        .expect("a failure event");
    assert_eq!((failed.0, failed.1), (8080, port));
    assert!(
        failed.2.contains("already in use by another program"),
        "{}",
        failed.2
    );
    let row = rig.row(8080, |_| true).await;
    assert!(row.pinned && !row.forwarded && row.host_port == Some(port));
    assert_eq!(row.problem.as_deref(), Some(failed.2.as_str()));
    // The user picks another port: the pin moves and the problem is gone.
    let other = free_port().await;
    let guest = echo().await;
    rig.pins.unpin(&rig.workspace, 8080).unwrap();
    rig.ports.release(&rig.workspace, 8080).await.unwrap();
    rig.ports
        .forward(&rig.workspace, guest, Some(other), true)
        .await
        .unwrap();
    assert_eq!(through(other, b"ok").await.unwrap(), b"ok");
    drop(busy);
}

#[tokio::test]
async fn a_forward_to_a_port_nothing_listens_on_resets_the_connection_and_says_why() {
    let rig = rig("nothing").await;
    rig.started().await;
    // Bound and never listening: connections to it are refused and no other test can take it.
    let unlistening = tokio::net::TcpSocket::new_v4().unwrap();
    unlistening.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let closed = unlistening.local_addr().unwrap().port();
    let host = free_port().await;
    rig.ports
        .forward(&rig.workspace, closed, Some(host), false)
        .await
        .unwrap();
    let back = through(host, b"x").await;
    assert!(back.is_err() || back.unwrap().is_empty());
    let row = rig.row(closed, |r| r.problem.is_some()).await;
    assert!(
        row.problem
            .unwrap()
            .contains("nothing in the workspace listens")
    );
    // The next connection that works clears the problem.
    let guest = echo().await;
    let host2 = free_port().await;
    rig.ports
        .forward(&rig.workspace, guest, Some(host2), false)
        .await
        .unwrap();
    through(host2, b"fine").await.unwrap();
    assert_eq!(rig.row(guest, |_| true).await.problem, None);
}

#[tokio::test]
async fn a_restarted_agent_gives_a_new_link_and_forwards_carry_on_through_it() {
    let mut rig = rig("restart").await;
    rig.started().await;
    let guest = echo().await;
    let host = free_port().await;
    rig.ports
        .forward(&rig.workspace, guest, Some(host), false)
        .await
        .unwrap();
    assert_eq!(through(host, b"one").await.unwrap(), b"one");
    // The agent goes away, as when the guest restarts in place; connections fail until it is back.
    rig.agent = None;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let down = through(host, b"lost").await;
    assert!(down.is_err() || down.unwrap().is_empty());
    rig.start_agent().await;
    let mut back = Vec::new();
    for _ in 0..100 {
        back = through(host, b"two").await.unwrap_or_default();
        if !back.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(back, b"two");
}

#[tokio::test]
async fn a_guest_that_opens_more_connections_than_the_limit_is_reset_not_served() {
    let rig = rig_with("limit", |p| p.with_connection_limit(2)).await;
    rig.started().await;
    // A guest server that holds its connections.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guest = listener.local_addr().unwrap().port();
    let held: Arc<Mutex<Vec<TcpStream>>> = Arc::default();
    let keep = Arc::clone(&held);
    tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            keep.lock().unwrap().push(conn);
        }
    });
    let host = free_port().await;
    rig.ports
        .forward(&rig.workspace, guest, Some(host), false)
        .await
        .unwrap();
    let mut clients = Vec::new();
    for _ in 0..2 {
        let mut c = TcpStream::connect(("127.0.0.1", host)).await.unwrap();
        c.write_all(b"x").await.unwrap();
        clients.push(c);
    }
    for _ in 0..500 {
        if held.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut third = TcpStream::connect(("127.0.0.1", host)).await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), third.read(&mut buf))
        .await
        .expect("the third was not reset");
    assert!(read.is_err() || read.unwrap() == 0);
    assert!(
        rig.row(guest, |r| r.problem.is_some())
            .await
            .problem
            .unwrap()
            .contains("too many")
    );
}

#[tokio::test]
async fn a_bad_port_a_stopped_workspace_and_a_broken_store_are_refused_in_words() {
    let rig = rig("refused").await;
    assert_eq!(
        rig.ports.forward(&rig.workspace, 0, None, false).await,
        Err(PortsError::BadPort)
    );
    assert_eq!(
        rig.ports.forward(&rig.workspace, 80, Some(0), false).await,
        Err(PortsError::BadPort)
    );
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, 80, Some(8080), false)
            .await,
        Err(PortsError::NotRunning)
    );
    assert!(
        PortsError::NotRunning
            .to_string()
            .contains("start it first")
    );
    rig.started().await;
    let guest = echo().await;
    let host = free_port().await;
    // A pin that cannot be saved closes the forward again, and so does an unpin that cannot.
    *rig.pins.read_only.lock().unwrap() = true;
    let err = rig
        .ports
        .forward(&rig.workspace, guest, Some(host), true)
        .await
        .unwrap_err();
    assert_eq!(err, PortsError::Pins("the database is read-only".into()));
    assert!(
        TcpStream::connect(("127.0.0.1", host)).await.is_err(),
        "the forward was closed again"
    );
    assert_eq!(
        rig.ports.view(&rig.workspace).unwrap().ports,
        Vec::<PortRow>::new()
    );
    *rig.pins.read_only.lock().unwrap() = false;
    rig.ports
        .forward(&rig.workspace, guest, Some(host), true)
        .await
        .unwrap();
    *rig.pins.read_only.lock().unwrap() = true;
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest, Some(host), false)
            .await,
        Err(PortsError::Pins("the database is read-only".into()))
    );
    assert_eq!(
        rig.ports.release(&rig.workspace, guest).await,
        Err(PortsError::Pins("the database is read-only".into()))
    );
    *rig.pins.read_only.lock().unwrap() = false;
    // A store that cannot even be read: the view says so, and a start announces it.
    *rig.pins.broken.lock().unwrap() = true;
    assert_eq!(
        rig.ports
            .forward(&rig.workspace, guest, Some(host), false)
            .await,
        Err(PortsError::Pins("the database is locked".into()))
    );
    assert!(rig.ports.view(&rig.workspace).is_err());
    rig.events();
    rig.started().await;
    assert!(rig.events().iter().any(|e| matches!(e, Event::WorkspacePortForwardFailed { reason, .. } if reason.contains("could not be restored"))));
}

#[tokio::test]
async fn the_same_listening_report_twice_announces_one_change() {
    let rig = rig("twice").await;
    assert_eq!(format!("{:?}", rig.ports), "Ports { .. }");
    rig.started().await;
    rig.events();
    GuestWatcher::ports(&rig.ports, &rig.workspace, vec![4000, 4001]);
    GuestWatcher::ports(&rig.ports, &rig.workspace, vec![4000, 4001]);
    let changes = rig
        .events()
        .into_iter()
        .filter(|e| matches!(e, Event::WorkspacePortsChanged { .. }))
        .count();
    assert_eq!(changes, 1);
}

#[tokio::test]
async fn a_forward_whose_workspace_has_no_agent_link_resets_and_says_the_agent_is_not_connected() {
    // No agent was started: nothing ever gives the workspace a link.
    let ports = Ports::with_programs(
        PuddleEndpoints::new(),
        Arc::new(MemoryPins::default()),
        Arc::new(CollectingSink::default()),
        Arc::new(Named(None)),
    );
    let workspace = name("shop");
    ports.started(&workspace).await;
    let host = free_port().await;
    ports
        .forward(&workspace, 3000, Some(host), false)
        .await
        .unwrap();
    let back = through(host, b"x").await;
    assert!(back.is_err() || back.unwrap().is_empty());
    let problem = ports.view(&workspace).unwrap().ports[0]
        .problem
        .clone()
        .unwrap();
    assert!(problem.contains("agent is not connected"), "{problem}");
}
