// SPDX-License-Identifier: GPL-3.0-or-later
//! The ports of a workspace, end to end: a real agent, the host's session code, `Ports` as the
//! watcher, and a "guest" that is this machine's loopback. What the user sees is the view and the
//! events; what a program sees is a listener on `127.0.0.1` that carries bytes.
#![expect(
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
use puddle_ports::{
    NoLink, Pin, Pins, PinsError, PortOwner, PortRow, Ports, PortsError, ProgramLookup,
};
use puddle_test_kit::accepts;
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
    let mut rig = rig_without_agent(tag, tweak);
    rig.start_agent().await;
    rig
}

/// The host side of a workspace whose agent has not started (and so has not said hello).
fn rig_without_agent(tag: &str, tweak: impl FnOnce(Ports) -> Ports) -> Rig {
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
    Rig {
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
    }
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
        puddle_test_kit::eventually(
            &format!("a row for {guest_port} that the test accepts"),
            Duration::from_secs(5),
            || {
                let view = self.ports.view(&self.workspace);
                view.as_ref()
                    .ok()
                    .and_then(|v| {
                        v.ports
                            .iter()
                            .find(|r| r.guest_port == guest_port && ok(r))
                            .cloned()
                    })
                    .ok_or_else(|| format!("{view:?}"))
            },
        )
        .await
    }

    /// Takes the events so far.
    fn events(&self) -> Vec<Event> {
        self.events.take()
    }

    async fn started(&self) {
        self.ports.started(&self.workspace).await;
    }
}

/// A port of this computer's loopback that refuses connections and that no other program can
/// take while the socket lives (bound, never listening).
fn unlistening() -> (tokio::net::TcpSocket, u16) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = socket.local_addr().unwrap().port();
    (socket, port)
}

/// `port` on this computer's loopback.
fn local(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// How many ports [`on_ports`] tries before it gives up.
const PORT_ATTEMPTS: u32 = 5;

/// A loopback port nobody held a moment ago.
// flake-ok: `Ports::forward` binds the number itself and a pin keeps it across a restart, so no
// listener can be handed over; `on_ports` retries when the number was taken first.
fn pick_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Runs `attempt` on a port from `pick` and returns the port with the result. A result of
/// `Taken` on that port means another program bound it between the pick and the attempt, so the
/// attempt runs again on a fresh port, up to [`PORT_ATTEMPTS`] times; every other result is
/// returned as it is.
async fn on_ports<T>(
    mut pick: impl FnMut() -> u16,
    mut attempt: impl AsyncFnMut(u16) -> Result<T, PortsError>,
) -> (u16, Result<T, PortsError>) {
    let mut last = (0, Err(PortsError::BadPort));
    for _ in 0..PORT_ATTEMPTS {
        let port = pick();
        let result = attempt(port).await;
        if !matches!(&result, Err(PortsError::Taken { host_port, .. }) if *host_port == port) {
            return (port, result);
        }
        last = (port, result);
    }
    last
}

/// [`on_ports`] on a port picked free just now.
async fn on_free_port<T>(
    attempt: impl AsyncFnMut(u16) -> Result<T, PortsError>,
) -> (u16, Result<T, PortsError>) {
    on_ports(pick_port, attempt).await
}

/// Forwards `guest` to a host port that was free a moment ago; returns that port and the result.
async fn forward_free(
    ports: &Ports,
    workspace: &WorkspaceName,
    guest: u16,
    pin: bool,
) -> (u16, Result<PortRow, PortsError>) {
    on_free_port(async |host| ports.forward(workspace, guest, Some(host), pin).await).await
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
    puddle_test_kit::eventually(
        "the closed port drops out of the list",
        Duration::from_secs(5),
        || rig.ports.view(&rig.workspace).unwrap().ports.len() == 1,
    )
    .await;
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
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, false).await;
    let row = forwarded.unwrap();
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
    assert!(!accepts(addr));
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
    let (first, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    forwarded.unwrap();
    let (second, moved) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    let moved = moved.unwrap();
    assert_eq!(moved.host_port, Some(second));
    assert!(!accepts(local(first)), "the old port is closed");
    assert_eq!(through(second, b"moved").await.unwrap(), b"moved");
    assert_eq!(
        rig.pins.pinned(&rig.workspace).unwrap(),
        [Pin {
            guest_port: guest,
            host_port: second
        }]
    );
    // No host port named: the guest's own number. It is free here only by luck, so forward a guest
    // port that was just free on this side. The echo server above is not on it, but a forward to
    // a port nothing listens on is still opened: connections to it fail and the row says why.
    let (same, row) =
        on_free_port(async |same| rig.ports.forward(&rig.workspace, same, None, false).await).await;
    assert_eq!(row.unwrap().host_port, Some(same));
}

#[tokio::test]
async fn a_pinned_forward_survives_a_stop_and_comes_back_at_the_next_start() {
    let rig = rig("pinned").await;
    rig.started().await;
    let guest = echo().await;
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    forwarded.unwrap();
    rig.ports.stopped(&rig.workspace).await;
    assert!(!accepts(local(host)));
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
    // Only the registry names this one, so the number is held and never bound by the forward.
    let (_own_held, own) = unlistening();
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
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, false).await;
    forwarded.unwrap();
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
    let (_pinned_held, pinned) = unlistening();
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
    let guest = echo().await;
    rig.pins.unpin(&rig.workspace, 8080).unwrap();
    rig.ports.release(&rig.workspace, 8080).await.unwrap();
    let (other, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    forwarded.unwrap();
    assert_eq!(through(other, b"ok").await.unwrap(), b"ok");
    drop(busy);
}

#[tokio::test]
async fn a_forward_to_a_port_nothing_listens_on_resets_the_connection_and_says_why() {
    let rig = rig("nothing").await;
    rig.started().await;
    let (_held, closed) = unlistening();
    let (host, forwarded) = on_free_port(async |host| {
        rig.ports
            .forward(&rig.workspace, closed, Some(host), false)
            .await
    })
    .await;
    forwarded.unwrap();
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
    let (host2, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, false).await;
    forwarded.unwrap();
    through(host2, b"fine").await.unwrap();
    assert_eq!(rig.row(guest, |_| true).await.problem, None);
}

#[tokio::test]
async fn a_restarted_agent_gives_a_new_link_and_forwards_carry_on_through_it() {
    let mut rig = rig("restart").await;
    rig.started().await;
    let guest = echo().await;
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, false).await;
    forwarded.unwrap();
    assert_eq!(through(host, b"one").await.unwrap(), b"one");
    // The agent goes away, as when the guest restarts in place; connections fail until it is back.
    rig.agent = None;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let down = through(host, b"lost").await;
    assert!(down.is_err() || down.unwrap().is_empty());
    rig.start_agent().await;
    let back = puddle_test_kit::eventually_async(
        "a connection through the restarted agent answers",
        Duration::from_secs(5),
        || async {
            let back = through(host, b"two").await.unwrap_or_default();
            (!back.is_empty()).then_some(back)
        },
    )
    .await;
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
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, false).await;
    forwarded.unwrap();
    let mut clients = Vec::new();
    for _ in 0..2 {
        let mut c = TcpStream::connect(("127.0.0.1", host)).await.unwrap();
        c.write_all(b"x").await.unwrap();
        clients.push(c);
    }
    puddle_test_kit::eventually("both clients are held", Duration::from_secs(5), || {
        held.lock().unwrap().len() == 2
    })
    .await;
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
    // A pin that cannot be saved closes the forward again, and so does an unpin that cannot.
    *rig.pins.read_only.lock().unwrap() = true;
    let (host, refused) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    assert_eq!(
        refused,
        Err(PortsError::Pins("the database is read-only".into()))
    );
    assert!(!accepts(local(host)), "the forward was closed again");
    assert_eq!(
        rig.ports.view(&rig.workspace).unwrap().ports,
        Vec::<PortRow>::new()
    );
    *rig.pins.read_only.lock().unwrap() = false;
    let (host, forwarded) = forward_free(&rig.ports, &rig.workspace, guest, true).await;
    forwarded.unwrap();
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
    let (host, forwarded) = forward_free(&ports, &workspace, 3000, false).await;
    forwarded.unwrap();
    let back = through(host, b"x").await;
    assert!(back.is_err() || back.unwrap().is_empty());
    let problem = ports.view(&workspace).unwrap().ports[0]
        .problem
        .clone()
        .unwrap();
    assert!(problem.contains("agent is not connected"), "{problem}");
}

#[tokio::test]
async fn a_forward_on_a_port_another_program_bound_first_is_tried_again_on_a_fresh_one() {
    let rig = rig("collision").await;
    rig.started().await;
    let guest = echo().await;
    // The other program holds the port the first pick returned.
    let squatter = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = squatter.local_addr().unwrap().port();
    let mut picks = vec![taken].into_iter();
    let mut tried = Vec::new();
    let (port, forwarded) = on_ports(
        || picks.next().unwrap_or_else(pick_port),
        async |host| {
            tried.push(host);
            rig.ports
                .forward(&rig.workspace, guest, Some(host), false)
                .await
        },
    )
    .await;
    assert_eq!(forwarded.unwrap().host_port, Some(port));
    assert_eq!(tried, [taken, port]);
    assert_eq!(through(port, b"again").await.unwrap(), b"again");
}

#[tokio::test]
async fn a_port_that_stays_taken_gives_up_after_five_tries_with_the_refusal() {
    let rig = rig("stays").await;
    rig.started().await;
    let guest = echo().await;
    let squatter = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let taken = squatter.local_addr().unwrap().port();
    let mut tries = 0;
    let (port, forwarded) = on_ports(
        || taken,
        async |host| {
            tries += 1;
            rig.ports
                .forward(&rig.workspace, guest, Some(host), false)
                .await
        },
    )
    .await;
    assert_eq!(tries, PORT_ATTEMPTS);
    assert_eq!(port, taken);
    assert!(matches!(forwarded, Err(PortsError::Taken { .. })));
}

#[tokio::test]
async fn a_link_asked_for_before_the_agents_hello_arrives_with_the_hello_and_not_with_a_timer() {
    let mut rig = rig_without_agent("late-hello", |ports| ports);
    rig.started().await;
    let ports = rig.ports.clone();
    let workspace = rig.workspace.clone();
    // A bound far above the test's own time: only the hello can end the wait early.
    let asked = tokio::spawn(async move {
        ports
            .guest_link_within(&workspace, Duration::from_secs(60))
            .await
            .map(|_| ())
    });
    // An absence window: nothing signals that the wait has not ended on its own.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!asked.is_finished(), "nothing said hello yet");
    rig.start_agent().await;
    assert_eq!(asked.await.unwrap(), Ok(()));
}

#[tokio::test]
async fn a_link_asked_for_of_a_running_workspace_that_never_hears_from_its_agent_says_so() {
    let rig = rig_without_agent("no-hello", |ports| ports);
    rig.started().await;
    let got = rig
        .ports
        .guest_link_within(&rig.workspace, Duration::from_millis(50))
        .await;
    assert_eq!(got.err(), Some(NoLink::NotConnected));
}

#[tokio::test]
async fn a_link_asked_for_of_a_workspace_that_stops_meanwhile_is_not_running() {
    let rig = rig_without_agent("stops", |ports| ports);
    assert_eq!(
        rig.ports
            .guest_link_within(&rig.workspace, Duration::from_secs(60))
            .await
            .err(),
        Some(NoLink::NotRunning),
        "never started"
    );
    rig.started().await;
    let ports = rig.ports.clone();
    let workspace = rig.workspace.clone();
    let asked = tokio::spawn(async move {
        ports
            .guest_link_within(&workspace, Duration::from_secs(60))
            .await
            .err()
    });
    // An absence window: nothing signals that the wait has not ended on its own.
    tokio::time::sleep(Duration::from_millis(100)).await;
    rig.ports.stopped(&rig.workspace).await;
    assert_eq!(asked.await.unwrap(), Some(NoLink::NotRunning));
}
