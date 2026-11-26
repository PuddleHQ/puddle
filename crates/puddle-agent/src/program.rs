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
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_agent_proto::program::Program;
use tokio::net::TcpStream;

/// How long a process's name is reused. A build tool opens thousands of connections from a few
/// processes; the descriptor scan still runs per connection (a socket's owner is not known
/// without it), but the name is read once per process.
const NAME_TTL: Duration = Duration::from_secs(5);

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
    names: Mutex<HashMap<u32, (Instant, Option<Program>)>>,
}

impl Resolver {
    fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            names: Mutex::new(HashMap::new()),
        }
    }

    fn resolve(&self, peer: SocketAddr) -> Option<Program> {
        let inode = self.socket_inode(canonical(peer))?;
        let pid = self.owner(inode)?;
        self.name_of(pid)
    }

    /// The inode of the socket whose local address is `peer`, which is the client's end of the
    /// connection.
    fn socket_inode(&self, peer: SocketAddr) -> Option<u64> {
        ["net/tcp", "net/tcp6"].iter().find_map(|table| {
            let text = fs::read_to_string(self.root.join(table)).ok()?;
            text.lines().skip(1).find_map(|row| inode_of_row(row, peer))
        })
    }

    /// The first process that holds a descriptor for socket `inode`.
    fn owner(&self, inode: u64) -> Option<u32> {
        let wanted = format!("socket:[{inode}]");
        fs::read_dir(&self.root).ok()?.flatten().find_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let holds = fs::read_dir(entry.path().join("fd"))
                .ok()?
                .flatten()
                .any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t.as_os_str() == wanted.as_str()));
            holds.then_some(pid)
        })
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

/// The socket inode of one `/proc/net/tcp{,6}` row when its local address is `peer`.
fn inode_of_row(row: &str, peer: SocketAddr) -> Option<u64> {
    let mut fields = row.split_whitespace();
    let local = parse_address(fields.nth(1)?)?;
    if canonical(local) != peer {
        return None;
    }
    // After the local address: remote, state, queues, timer, retransmits, uid, timeout, inode.
    fields.nth(7)?.parse().ok()
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
