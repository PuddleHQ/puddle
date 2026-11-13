// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ending the editor servers' extension hosts when the host asks (the sandbox's editor window
//! closed), and sweeping debuggees nobody can reach any more.
//!
//! A closed window leaves its extension host running: about 0.7 GB, and a debug session's
//! debuggee under it, until the server's reconnection grace ends the host (and orphans the
//! debuggee). The cleanup is `TERM` to every extension host and its process tree, `KILL` after
//! [`TERM_GRACE`] to what is left.
//!
//! - **Which processes:** an extension host is a process with the argument `--type=extensionHost`
//!   that names a file below one of the servers' folders. Its tree is everything below it by parent
//!   id (language servers, debuggees). The servers' pty hosts are not below it, so terminals and
//!   their jobs stay.
//! - **Orphan sweep:** a process a debug session launched carries `VSCODE_INSPECTOR_OPTIONS` (with
//!   `deferredMode` off; a terminal's auto attach sets it on and is left alone); when its
//!   parent is gone (it is the child of process 1) and the socket the options name for the debug
//!   session no longer exists, nothing can reach it and it is ended. A job the user left running
//!   on purpose from a terminal carries the variable only when auto attach is on, and then its
//!   socket exists while its extension host lives.
//!
//! The process table is read from `/proc` (Linux only; elsewhere nothing is found). Everything
//! that picks processes works on a [`Proc`] list, so the choice is tested without processes.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The argument that marks an extension host.
const HOST_ARG: &str = "--type=extensionHost";
/// The variable js-debug puts in a debuggee's environment.
#[cfg(target_os = "linux")]
const INSPECTOR_VAR: &str = "VSCODE_INSPECTOR_OPTIONS";

/// How long a `TERM`ed tree has before it is `KILL`ed.
pub const TERM_GRACE: Duration = Duration::from_secs(3);
/// How often the tree is looked at while waiting.
const POLL: Duration = Duration::from_millis(50);
/// How many [`POLL`]s a killed process gets to leave the table.
const SETTLE_POLLS: u32 = 40;
/// How often the orphan sweep runs while the editor server is on.
pub const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// One process, as far as this module needs to know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proc {
    /// The process id.
    pub pid: i32,
    /// The parent's id (1 once the parent is gone).
    pub ppid: i32,
    /// When it started (clock ticks since boot): with the id, tells a process from a later one
    /// that got the same id.
    pub start: u64,
    /// Dead but not yet collected by its parent: counts as gone.
    pub zombie: bool,
    /// The command line.
    pub argv: Vec<String>,
    /// `VSCODE_INSPECTOR_OPTIONS`, read only for the children of process 1.
    pub inspector: Option<String>,
}

/// What a cleanup ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ended {
    /// Extension hosts found.
    pub hosts: u32,
    /// Processes ended in their trees, the hosts included.
    pub processes: u32,
    /// Orphaned debuggees ended.
    pub orphans: u32,
}

/// The ids of the extension hosts of the servers installed in `servers`.
///
/// A child a host has forked shows the host's command line until it runs its own program, so a
/// match whose parent matches too is that child, part of its parent's tree, not another host.
#[must_use]
pub fn extension_hosts(procs: &[Proc], servers: &[PathBuf]) -> Vec<i32> {
    let matches: HashSet<i32> = procs
        .iter()
        .filter(|p| !p.zombie)
        .filter(|p| p.argv.iter().any(|a| a == HOST_ARG))
        .filter(|p| {
            p.argv
                .iter()
                .any(|a| servers.iter().any(|s| Path::new(a).starts_with(s)))
        })
        .map(|p| p.pid)
        .collect();
    procs
        .iter()
        .filter(|p| matches.contains(&p.pid) && !matches.contains(&p.ppid))
        .map(|p| p.pid)
        .collect()
}

/// `roots` and every process below them.
#[must_use]
pub fn tree(procs: &[Proc], roots: &[i32]) -> HashSet<i32> {
    let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p.pid);
    }
    let mut found: HashSet<i32> = HashSet::new();
    let mut todo: Vec<i32> = roots.to_vec();
    while let Some(pid) = todo.pop() {
        if found.insert(pid) {
            todo.extend(children.get(&pid).into_iter().flatten());
        }
    }
    found
}

/// The path a debug session's inspector options name as its socket, `None` when they name none.
fn inspector_socket(options: &str) -> Option<PathBuf> {
    let value: serde_json::Value = serde_json::from_str(options).ok()?;
    // Only a program a debug session launched itself is a debuggee. A terminal's auto attach sets the
    // same variable with `deferredMode` on, and what it started may be a job the user means to keep.
    if value.get("deferredMode")?.as_bool()? {
        return None;
    }
    value.get("inspectorIpc")?.as_str().map(PathBuf::from)
}

/// The ids of debuggees nobody can reach: children of `reaper` (process 1 in the guest) whose inspector options name a
/// debug socket that is gone and that a debug session launched (`deferredMode` off; `socket_exists` answers for a path). Options that name no socket
/// are left alone: nothing says the process is orphaned.
#[must_use]
pub fn orphan_debuggees(
    procs: &[Proc],
    reaper: i32,
    socket_exists: impl Fn(&Path) -> bool,
) -> Vec<i32> {
    procs
        .iter()
        .filter(|p| p.ppid == reaper && !p.zombie)
        .filter(|p| {
            p.inspector
                .as_deref()
                .and_then(inspector_socket)
                .is_some_and(|socket| !socket_exists(&socket))
        })
        .map(|p| p.pid)
        .collect()
}

/// Ends the extension hosts of `servers` and what they started, then sweeps orphaned debuggees.
/// Returns when no process of the trees is left (after at most [`TERM_GRACE`] plus a short
/// wait for the `KILL`s to take effect).
pub async fn end_extension_hosts(servers: &[PathBuf]) -> Ended {
    end_extension_hosts_within(servers, TERM_GRACE).await
}

async fn end_extension_hosts_within(servers: &[PathBuf], grace: Duration) -> Ended {
    let procs = snapshot();
    let hosts = extension_hosts(&procs, servers);
    let mut members: HashMap<i32, u64> = HashMap::new();
    let mut ended = Ended {
        hosts: u32::try_from(hosts.len()).unwrap_or(u32::MAX),
        ..Ended::default()
    };
    if !hosts.is_empty() {
        absorb(&mut members, &procs, &hosts);
        signal_all(&members.keys().copied().collect::<Vec<_>>(), Signal::Term);
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let now = snapshot();
            let alive = living(&members, &now);
            if alive.is_empty() {
                break;
            }
            // A process a host started while it was ending is part of the tree too.
            absorb(&mut members, &now, &alive);
            if tokio::time::Instant::now() >= deadline {
                signal_all(&alive, Signal::Kill);
                kill_settle(&members).await;
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        ended.processes = u32::try_from(members.len()).unwrap_or(u32::MAX);
    }
    ended.orphans = sweep_orphans().await;
    ended
}

/// Adds `roots` and everything below them in `procs` to `members` (by id, with the start time).
fn absorb(members: &mut HashMap<i32, u64>, procs: &[Proc], roots: &[i32]) {
    let below = tree(procs, roots);
    for p in procs.iter().filter(|p| below.contains(&p.pid)) {
        members.entry(p.pid).or_insert(p.start);
    }
}

/// The members still running in `procs`: same id and start time, not a zombie.
fn living(members: &HashMap<i32, u64>, procs: &[Proc]) -> Vec<i32> {
    procs
        .iter()
        .filter(|p| !p.zombie && members.get(&p.pid) == Some(&p.start))
        .map(|p| p.pid)
        .collect()
}

/// Waits briefly for processes that got `KILL` to leave the table.
async fn kill_settle(members: &HashMap<i32, u64>) {
    for _ in 0..SETTLE_POLLS {
        if living(members, &snapshot()).is_empty() {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Ends the orphaned debuggees now; returns how many.
pub async fn sweep_orphans() -> u32 {
    sweep_orphans_of(1).await
}

/// [`sweep_orphans`] for orphans that `reaper` adopted (process 1 in the guest; a test that made
/// itself the reaper of its descendants names its own id).
pub async fn sweep_orphans_of(reaper: i32) -> u32 {
    let procs = snapshot_of(reaper);
    let orphans = orphan_debuggees(&procs, reaper, Path::exists);
    if orphans.is_empty() {
        return 0;
    }
    let mut members = HashMap::new();
    absorb(&mut members, &procs, &orphans);
    signal_all(&members.keys().copied().collect::<Vec<_>>(), Signal::Term);
    for _ in 0..(TERM_GRACE.as_millis() / POLL.as_millis()) {
        if living(&members, &snapshot()).is_empty() {
            return u32::try_from(orphans.len()).unwrap_or(u32::MAX);
        }
        tokio::time::sleep(POLL).await;
    }
    signal_all(&living(&members, &snapshot()), Signal::Kill);
    kill_settle(&members).await;
    u32::try_from(orphans.len()).unwrap_or(u32::MAX)
}

/// Looks for the debuggees `reaper` adopted every [`SWEEP_EVERY`], for as long as the agent runs
/// (process 1 in the guest).
pub async fn sweep_loop(reaper: i32) {
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        let n = sweep_orphans_of(reaper).await;
        if n > 0 {
            tracing::info!(count = n, "ended orphaned debuggees");
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(target_os = "linux")]
fn signal_all(pids: &[i32], signal: Signal) {
    use rustix::process::{Pid, Signal as Sig, kill_process};
    let sig = match signal {
        Signal::Term => Sig::TERM,
        Signal::Kill => Sig::KILL,
    };
    for pid in pids {
        // A process that already left is not an error; nothing else can go wrong for the agent (root).
        if let Some(pid) = Pid::from_raw(*pid) {
            let _ = kill_process(pid, sig);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn signal_all(_pids: &[i32], _signal: Signal) {}

/// Reads the process table from `/proc`.
#[must_use]
pub fn snapshot() -> Vec<Proc> {
    snapshot_of(1)
}

/// [`snapshot`], reading the inspector options of the children of `reaper` instead of process 1.
#[cfg(target_os = "linux")]
#[must_use]
pub fn snapshot_of(reaper: i32) -> Vec<Proc> {
    // A `/proc` that cannot be read is an empty table, the same as off Linux.
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(|pid| read_proc(pid, reaper))
        .collect()
}

/// Nothing is found off Linux.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn snapshot_of(_reaper: i32) -> Vec<Proc> {
    Vec::new()
}

/// The fields of `/proc/<pid>/stat` after the command name: state, ppid and start time.
#[cfg(target_os = "linux")]
fn parse_stat(stat: &str) -> Option<(bool, i32, u64)> {
    // The command name is in parentheses and may hold spaces and parentheses itself.
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let mut fields = rest.split_whitespace();
    let status = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    // The start time is field 22; the status and the parent were fields 3 and 4.
    let started = fields.nth(17)?.parse().ok()?;
    Some((status == "Z", parent, started))
}

#[cfg(target_os = "linux")]
fn read_proc(pid: i32, reaper: i32) -> Option<Proc> {
    let base = PathBuf::from(format!("/proc/{pid}"));
    let (zombie, parent, started) = parse_stat(&std::fs::read_to_string(base.join("stat")).ok()?)?;
    let argv = std::fs::read(base.join("cmdline"))
        .map(|raw| {
            raw.split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default();
    let inspector = (parent == reaper)
        .then(|| inspector_option(&base))
        .flatten();
    Some(Proc {
        pid,
        ppid: parent,
        start: started,
        zombie,
        argv,
        inspector,
    })
}

#[cfg(target_os = "linux")]
fn inspector_option(base: &Path) -> Option<String> {
    let raw = std::fs::read(base.join("environ")).ok()?;
    raw.split(|b| *b == 0).find_map(|var| {
        let var = String::from_utf8_lossy(var);
        var.strip_prefix(INSPECTOR_VAR)?
            .strip_prefix('=')
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests;
