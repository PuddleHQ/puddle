// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which program opened a connection to the agent's listeners.
//!
//! The peer address of an accepted connection names the client's socket in the guest's TCP tables
//! (`/proc/net/tcp`, `/proc/net/tcp6`: the row whose local address is the peer). The socket's inode
//! leads to the process that holds it (`/proc/<pid>/fd/*` pointing at `socket:[inode]`), and the
//! process leads to a name. The agent runs as root, so it can read every process's descriptors.
//!
//! The name is the basename of `/proc/<pid>/exe`. When that is an interpreter (`python3.12`,
//! `sh`) and the process's `comm` is something else, `comm` is the better name: a script started
//! through its shebang is named after the script (`pip`), while `node` renames its main thread to
//! `MainThread`, which is no name at all. A client in a container lives in another network
//! namespace, so its socket is not in the tables and it has no program.
//!
//! The proxy gets the answer as [`puddle_agent_proto::program::PROGRAM_HEADER`]. It is a display
//! hint, never an input to a rule. Nothing but the name leaves the guest: no command line, no
//! environment, no pid.

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_agent_proto::program::Program;
use tokio::net::TcpStream;

/// How long a process's name is reused. A build tool opens thousands of connections from a few
/// processes; the name is read once per process, and one scan of the descriptors (see [`Scans`])
/// serves every connection that was waiting for it.
const NAME_TTL: Duration = Duration::from_secs(5);

/// The longest a connection queues behind a running scan, judged by how long the last one took.
const WAIT_BUDGET: Duration = Duration::from_millis(20);

/// The cache is emptied when it holds this many processes, so short-lived processes cannot grow it.
const CACHE_MAX: usize = 512;

/// Interpreters whose `exe` says nothing about the script they run.
const INTERPRETERS: [&str; 7] = ["python", "ruby", "perl", "sh", "bash", "dash", "zsh"];

/// The program of `client`, found on a blocking thread. `None` when it cannot be told.
pub async fn of_client(client: &TcpStream) -> Option<Program> {
    let peer = client.peer_addr().ok()?;
    tokio::task::spawn_blocking(move || RESOLVER.resolve(peer))
        .await
        .ok()
        .flatten()
}

static RESOLVER: LazyLock<Resolver> = LazyLock::new(|| Resolver::new("/proc"));

/// Resolves client addresses to program names through a `/proc` tree.
struct Resolver {
    root: PathBuf,
    scans: Arc<(Mutex<Scans>, Condvar)>,
    wait_budget: Duration,
    names: Mutex<HashMap<u32, (Instant, Option<Program>)>>,
}

/// What one pass over `/proc` found.
#[derive(Default)]
struct Table {
    /// Socket inode by the local address the TCP tables list it under.
    sockets: HashMap<SocketAddr, u64>,
    /// The first process holding a descriptor for each socket inode.
    owners: HashMap<u64, u32>,
}

/// The passes over `/proc`: how many began and ended, whether one runs, and the last result.
#[derive(Default)]
struct Scans {
    started: u64,
    finished: u64,
    running: bool,
    /// How long the last pass took.
    last_cost: Duration,
    latest: Arc<Table>,
}

impl Resolver {
    fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            scans: Arc::default(),
            wait_budget: WAIT_BUDGET,
            names: Mutex::new(HashMap::new()),
        }
    }

    fn resolve(&self, peer: SocketAddr) -> Option<Program> {
        let table = self.table_after_now()?;
        let pid = *table.owners.get(table.sockets.get(&canonical(peer))?)?;
        self.name_of(pid)
    }

    /// A table read after this call began, so the connection that asks for it is in it. Scans run
    /// one at a time and each serves every caller that arrived before it started, so the cost of
    /// the walk over `/proc` follows the time that passes and not the number of connections: a
    /// tool that opens 2048 sockets at once pays for a handful of scans, never 2048.
    ///
    /// The kernel prints every TCP socket for a read of the table, closed ones waiting out
    /// `TIME_WAIT` included, so under a flood of short connections one scan takes tens of
    /// milliseconds. A caller that finds a scan running and the last one dearer than
    /// [`WAIT_BUDGET`] goes without a name instead of queueing behind it: the name is a hint, and a
    /// connection must not wait for one.
    fn table_after_now(&self) -> Option<Arc<Table>> {
        let (lock, wake) = &*self.scans;
        let mut scans = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let wanted = scans.started + 1;
        while scans.finished < wanted {
            if scans.running {
                if scans.last_cost > self.wait_budget {
                    return None;
                }
                scans = wake.wait(scans).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            scans.running = true;
            scans.started += 1;
            let number = scans.started;
            drop(scans);
            let began = Instant::now();
            let table = Arc::new(self.read_table());
            scans = lock.lock().unwrap_or_else(PoisonError::into_inner);
            scans.latest = table;
            scans.finished = number;
            scans.last_cost = began.elapsed();
            scans.running = false;
            wake.notify_all();
        }
        Some(Arc::clone(&scans.latest))
    }

    /// Every socket in the guest's TCP tables by local address, and the first process that holds
    /// each socket descriptor.
    fn read_table(&self) -> Table {
        let mut sockets = HashMap::new();
        for name in ["net/tcp", "net/tcp6"] {
            let Ok(text) = fs::read_to_string(self.root.join(name)) else {
                continue;
            };
            for (local, inode) in text.lines().skip(1).filter_map(table_row) {
                sockets.entry(canonical(local)).or_insert(inode);
            }
        }
        let mut owners = HashMap::new();
        let processes = fs::read_dir(&self.root).into_iter().flatten().flatten();
        for entry in processes {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                let inode = fs::read_link(fd.path()).ok().and_then(|target| {
                    let target = target.to_str()?;
                    target
                        .strip_prefix("socket:[")?
                        .strip_suffix(']')?
                        .parse::<u64>()
                        .ok()
                });
                if let Some(inode) = inode {
                    owners.entry(inode).or_insert(pid);
                }
            }
        }
        Table { sockets, owners }
    }

    fn name_of(&self, pid: u32) -> Option<Program> {
        let now = Instant::now();
        let mut names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, name)) = names.get(&pid)
            && now.duration_since(*at) < NAME_TTL
        {
            return name.clone();
        }
        let name = self.read_name(pid);
        if names.len() >= CACHE_MAX {
            names.clear();
        }
        names.insert(pid, (now, name.clone()));
        name
    }

    fn read_name(&self, pid: u32) -> Option<Program> {
        let dir: &Path = &self.root.join(pid.to_string());
        let comm = fs::read_to_string(dir.join("comm")).ok();
        let exe = fs::read_link(dir.join("exe")).ok().and_then(|path| {
            let base = path.file_name()?.to_str()?;
            // A replaced binary reads as `/usr/bin/node (deleted)`.
            Some(base.trim_end_matches(" (deleted)").to_owned())
        });
        Program::sanitise(&pick_name(exe.as_deref(), comm.as_deref().map(str::trim)))
    }
}

/// The name to show, from the basename of the executable and the process's `comm`.
fn pick_name(exe: Option<&str>, comm: Option<&str>) -> String {
    let comm = comm.filter(|c| !c.is_empty());
    match (exe, comm) {
        (Some(exe), Some(comm)) if is_interpreter(exe) && comm != exe && comm != "MainThread" => {
            comm.to_owned()
        }
        (Some(exe), _) => exe.to_owned(),
        (None, Some(comm)) => comm.to_owned(),
        (None, None) => String::new(),
    }
}

fn is_interpreter(exe: &str) -> bool {
    INTERPRETERS.iter().any(|name| {
        exe.strip_prefix(name)
            .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
    })
}

/// An IPv4-mapped address is the IPv4 address: a dual-stack socket reports it either way.
fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// The local address and socket inode of one `/proc/net/tcp{,6}` row.
fn table_row(row: &str) -> Option<(SocketAddr, u64)> {
    let mut fields = row.split_whitespace();
    let local = parse_address(fields.nth(1)?)?;
    // After the local address: remote, state, queues, timer, retransmits, uid, timeout, inode.
    Some((local, fields.nth(7)?.parse().ok()?))
}

/// `0100007F:1F90` (IPv4) or `00000000000000000000000001000000:1F90` (IPv6): the address as the
/// kernel prints it, each 32-bit word in host byte order (little endian on every guest we run),
/// then the port in hex.
fn parse_address(text: &str) -> Option<SocketAddr> {
    let (ip, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(ip, 16).ok()?.to_le_bytes(),
        )),
        32 => {
            let word = |i: usize| u32::from_str_radix(ip.get(i * 8..i * 8 + 8)?, 16).ok();
            let mut octets = [0u8; 16];
            for (i, chunk) in octets.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                *chunk = word(i)?.to_le_bytes();
            }
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests;
