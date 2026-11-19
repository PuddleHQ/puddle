// SPDX-License-Identifier: AGPL-3.0-or-later
//! A userspace stand-in for a bad network link, in front of one TCP server: no root, no
//! namespace. Tests point Puddle's resolver at the shim's port and then script the link:
//! cut it (connections are refused), blackhole it (a connect hangs), pause a download for a while,
//! reset one in the middle (a TCP `RST`, what a dropped route or a crashed middlebox does), or
//! add the delay and the retransmission stalls a lossy link shows to the application above TCP.
//!
//! The shim cannot drop packets: loss is shown the way TCP shows it to a program, as a chunk that
//! arrives one retransmission timeout late. The kernel's own behaviour under loss is the
//! namespace tests' job (`stall_meter.rs`).
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::task::JoinHandle;

/// How the link treats the next connections. One-shot actions fire once, on the first
/// connection whose download reaches the byte count.
#[derive(Default)]
struct Plan {
    /// Stop the download for this long once this many bytes came down.
    pause: Option<(u64, Duration)>,
    /// Reset both ends once this many bytes came down.
    reset: Option<u64>,
    /// Added before every chunk, in both directions.
    delay: Duration,
    /// Every this-many chunks (0: never) is held for `stall` more, a retransmission timeout.
    loss_every: u64,
    stall: Duration,
}

#[derive(Default)]
struct Counters {
    connections: AtomicU64,
    bytes_down: AtomicU64,
    chunks: AtomicU64,
    stalled: AtomicU64,
}

/// What a link did, for a test to check that its script ran.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Carried {
    pub(crate) connections: u64,
    pub(crate) bytes_down: u64,
    pub(crate) stalled_chunks: u64,
}

enum Door {
    Open(JoinHandle<()>),
    /// A listener that never accepts, its queue full: a connect to it hangs.
    Blackhole {
        _listener: TcpListener,
        _fillers: Vec<std::net::TcpStream>,
    },
    Closed,
}

/// The link. Its port stays the same through [`Self::cut`] and [`Self::restore`].
pub(crate) struct LinkShim {
    addr: SocketAddr,
    target: SocketAddr,
    plan: Arc<Mutex<Plan>>,
    counters: Arc<Counters>,
    door: Mutex<Door>,
    live: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl Drop for LinkShim {
    fn drop(&mut self) {
        if let Door::Open(task) = &*self.door.lock().unwrap_or_else(PoisonError::into_inner) {
            task.abort();
        }
        for task in self.live.lock().unwrap_or_else(PoisonError::into_inner).iter() {
            task.abort();
        }
    }
}

fn bind(addr: SocketAddr, backlog: u32) -> std::io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(backlog)
}

/// [`bind`], retried while an aborted task has not yet dropped the old listener.
async fn rebind(addr: SocketAddr, backlog: u32) -> TcpListener {
    for _ in 0..100 {
        if let Ok(listener) = bind(addr, backlog) {
            return listener;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bind(addr, backlog).unwrap()
}

impl LinkShim {
    /// A link in front of `target`, open.
    pub(crate) fn start(target: SocketAddr) -> Self {
        let listener = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), 128).unwrap();
        let shim = Self {
            addr: listener.local_addr().unwrap(),
            target,
            plan: Arc::default(),
            counters: Arc::default(),
            door: Mutex::new(Door::Closed),
            live: Arc::default(),
        };
        shim.open(listener);
        shim
    }

    /// Where clients connect.
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    fn open(&self, listener: TcpListener) {
        let (plan, counters, live, target) = (
            Arc::clone(&self.plan),
            Arc::clone(&self.counters),
            Arc::clone(&self.live),
            self.target,
        );
        let task = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                counters.connections.fetch_add(1, Ordering::SeqCst);
                let carry = carry(client, target, Arc::clone(&plan), Arc::clone(&counters));
                live.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(tokio::spawn(carry));
            }
        });
        *self.door.lock().unwrap_or_else(PoisonError::into_inner) = Door::Open(task);
    }

    fn close_door(&self) {
        let mut door = self.door.lock().unwrap_or_else(PoisonError::into_inner);
        if let Door::Open(task) = &*door {
            task.abort();
        }
        for task in self.live.lock().unwrap_or_else(PoisonError::into_inner).drain(..) {
            task.abort();
        }
        *door = Door::Closed;
    }

    /// The route is gone: running transfers end and a new connect is refused.
    pub(crate) async fn cut(&self) {
        self.close_door();
        // The aborted tasks dropped their sockets and the listener; wait until the port refuses.
        for _ in 0..100 {
            if TcpStream::connect(self.addr).await.is_err() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("the cut link still accepts");
    }

    /// The route drops every packet: a connect to it hangs until the client gives up.
    pub(crate) async fn blackhole(&self) {
        self.close_door();
        let listener = rebind(self.addr, 0).await;
        // The accept queue holds one or two connections; once it is full the kernel drops the SYN.
        let mut fillers = Vec::new();
        for _ in 0..4 {
            if let Ok(conn) =
                std::net::TcpStream::connect_timeout(&self.addr, Duration::from_millis(200))
            {
                fillers.push(conn);
            }
        }
        *self.door.lock().unwrap_or_else(PoisonError::into_inner) =
            Door::Blackhole {
            _listener: listener,
            _fillers: fillers,
        };
    }

    /// The route is back.
    pub(crate) async fn restore(&self) {
        self.close_door();
        self.open(rebind(self.addr, 128).await);
    }

    /// Stops the first download that reaches `after` bytes for `how_long`, then lets it go on.
    pub(crate) fn pause_once(&self, after: u64, how_long: Duration) {
        self.plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pause = Some((after, how_long));
    }

    /// Resets the first download that reaches `after` bytes, both ends.
    pub(crate) fn reset_once(&self, after: u64) {
        self.plan
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .reset = Some(after);
    }

    /// A slow, lossy link: every chunk takes `delay` more, and every `loss_every`th one waits
    /// `retransmit` more.
    pub(crate) fn lossy(&self, delay: Duration, loss_every: u64, retransmit: Duration) {
        let mut plan = self.plan.lock().unwrap_or_else(PoisonError::into_inner);
        plan.delay = delay;
        plan.loss_every = loss_every;
        plan.stall = retransmit;
    }

    pub(crate) fn carried(&self) -> Carried {
        Carried {
            connections: self.counters.connections.load(Ordering::SeqCst),
            bytes_down: self.counters.bytes_down.load(Ordering::SeqCst),
            stalled_chunks: self.counters.stalled.load(Ordering::SeqCst),
        }
    }
}

/// What happens before a chunk of `plan`'s link moves on.
async fn hold(plan: &Mutex<Plan>, counters: &Counters) {
    let (delay, every, stall) = {
        let plan = plan.lock().unwrap_or_else(PoisonError::into_inner);
        (plan.delay, plan.loss_every, plan.stall)
    };
    let n = counters.chunks.fetch_add(1, Ordering::SeqCst) + 1;
    let mut wait = delay;
    if every > 0 && n % every == 0 {
        counters.stalled.fetch_add(1, Ordering::SeqCst);
        wait += stall;
    }
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

async fn carry(
    mut client: TcpStream,
    target: SocketAddr,
    plan: Arc<Mutex<Plan>>,
    counters: Arc<Counters>,
) {
    let Ok(mut server) = TcpStream::connect(target).await else {
        return;
    };
    let mut up = vec![0_u8; 16 * 1024];
    let mut down = vec![0_u8; 16 * 1024];
    let mut sent = 0_u64;
    loop {
        tokio::select! {
            read = client.read(&mut up) => {
                let n = read.unwrap_or(0);
                if n == 0 {
                    let _ = server.shutdown().await;
                    // The server may still answer: carry on until it closes.
                    let _ = tokio::io::copy(&mut server, &mut client).await;
                    let _ = client.shutdown().await;
                    return;
                }
                hold(&plan, &counters).await;
                if server.write_all(&up[..n]).await.is_err() {
                    return;
                }
            }
            read = server.read(&mut down) => {
                let n = match read {
                    Ok(0) | Err(_) => {
                        let _ = client.shutdown().await;
                        return;
                    }
                    Ok(n) => n,
                };
                hold(&plan, &counters).await;
                let (pause, reset) = {
                    let mut plan = plan.lock().unwrap_or_else(PoisonError::into_inner);
                    let reached = |after: u64| sent + n as u64 >= after;
                    let pause = plan.pause.filter(|(after, _)| reached(*after));
                    if pause.is_some() {
                        plan.pause = None;
                    }
                    let reset = plan.reset.filter(|after| reached(*after));
                    if reset.is_some() {
                        plan.reset = None;
                    }
                    (pause, reset)
                };
                if let Some(after) = reset {
                    // The bytes up to the cut go out, then both ends reset: the client reads data
                    // and then an error, never an end of file.
                    let keep = usize::try_from(after.saturating_sub(sent)).unwrap_or(0).min(n);
                    let _ = client.write_all(&down[..keep]).await;
                    let _ = client.flush().await;
                    let _ = client.set_zero_linger();
                    let _ = server.set_zero_linger();
                    return;
                }
                if client.write_all(&down[..n]).await.is_err() {
                    return;
                }
                sent += n as u64;
                counters.bytes_down.fetch_add(n as u64, Ordering::SeqCst);
                if let Some((_, how_long)) = pause {
                    tokio::time::sleep(how_long).await;
                }
            }
        }
    }
}
