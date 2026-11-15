// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a fixture's processes are doing: the tree under the process a fixture started, with each
//! process's start time and CPU time, and how long the machine takes to start a trivial program.
//!
//! A fixture start that misses its window reports these, so its failure says which of three it
//! was: the program had not been started yet (a later process of the tree is missing or started
//! late), it was started and worked (CPU time), or it was started and waited (no CPU time); and
//! whether starting any program was slow at that moment, and how much memory was free (a machine
//! out of memory pages every new program in from disk, which on a hosted runner took 14 to 35 s).

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// One process of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Proc {
    pid: u32,
    parent: u32,
    name: String,
    /// When it was created; `None` when the OS does not say.
    created: Option<SystemTime>,
    /// User plus system CPU time so far; `None` when the OS does not say.
    cpu: Option<Duration>,
}

/// `root` and every process descended from it in `all`. A process whose creation time is known
/// and earlier than its parent's has a reused parent id: it is not that parent's child.
fn descendants(all: &[Proc], root: u32) -> Vec<&Proc> {
    let Some(first) = all.iter().find(|p| p.pid == root) else {
        return Vec::new();
    };
    let mut tree = vec![first];
    let mut next = 0;
    while let Some(&parent) = tree.get(next) {
        next += 1;
        for p in all {
            let born_after = match (parent.created, p.created) {
                (Some(parent), Some(child)) => child >= parent,
                _ => true,
            };
            if p.parent == parent.pid && p.pid != parent.pid && born_after {
                tree.push(p);
            }
        }
    }
    tree
}

/// The processes of the tree under `root`, `root` first.
fn tree(root: u32) -> Vec<Proc> {
    let all = os::snapshot();
    descendants(&all, root).into_iter().cloned().collect()
}

/// The CPU time of the tree under `root` (the fixture's own process and the ones it started:
/// a Windows console-script launcher does no work of its own), or `None` when the OS does not
/// say.
pub(super) fn tree_cpu(root: u32) -> Option<Duration> {
    let tree = tree(root);
    if tree.is_empty() {
        return None;
    }
    tree.iter().map(|p| p.cpu).sum()
}

/// One line per process of `tree`: name, pid, its start relative to the first one, and CPU.
fn describe(tree: &[Proc]) -> String {
    let Some(first) = tree.first() else {
        return "the process tree is not readable on this OS, or the process has ended".to_owned();
    };
    tree.iter()
        .map(|p| {
            let start = match (first.created, p.created) {
                (Some(first), Some(this)) => this.duration_since(first).map_or_else(
                    |_| "before the first".to_owned(),
                    |after| format!("+{:.2}s", after.as_secs_f64()),
                ),
                _ => "unknown".to_owned(),
            };
            let cpu = p.cpu.map_or_else(
                || "unknown".to_owned(),
                |c| format!("{:.2}s", c.as_secs_f64()),
            );
            format!("  {} pid {} started {start} CPU {cpu}", p.name, p.pid)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How long [`trivial_start`] waits for its program to end.
const TRIVIAL_START_BOUND: Duration = Duration::from_secs(30);

/// Starts a program that does nothing (`cmd /c exit 0`, `true`) and says how long it took to run
/// to its end. Idle it takes milliseconds; a machine that stalls every program start shows here.
fn trivial_start() -> String {
    let (program, args) = TRIVIAL;
    time_to_end(program, args, TRIVIAL_START_BOUND)
}

/// A program that does nothing and ends, with its arguments.
#[cfg(windows)]
const TRIVIAL: (&str, &[&str]) = ("cmd", &["/c", "exit 0"]);
#[cfg(not(windows))]
const TRIVIAL: (&str, &[&str]) = ("true", &[]);

/// Programs the tests run: one that waits about 30 s using no CPU, one that spins until stopped,
/// and a shell that waits on a child of its own (the child's executable name last).
#[cfg(test)]
pub(super) mod programs;

/// Runs `program` and says how long it took from the call to its end, or that it had not ended
/// within `bound` (it is then stopped).
fn time_to_end(program: &str, args: &[&str], bound: Duration) -> String {
    let what = std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ");
    let begun = Instant::now();
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return format!("a trivial program ({what}) did not start: {e}"),
    };
    let spawned = begun.elapsed();
    // flake-ok: this measures how long the program takes to end and returns the figure, so it is not a wait that fails on a bound
    loop {
        if let Ok(Some(_)) = child.try_wait() {
            return format!(
                "a trivial program ({what}) took {:.2}s to start and end ({:.2}s in spawn)",
                begun.elapsed().as_secs_f64(),
                spawned.as_secs_f64()
            );
        }
        if begun.elapsed() >= bound {
            drop(child.kill());
            drop(child.wait());
            return format!(
                "a trivial program ({what}) had not ended after {:.1}s ({:.2}s in spawn)",
                bound.as_secs_f64(),
                spawned.as_secs_f64()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What `child`'s process tree is doing and how long the machine takes to start a trivial
/// program right now: the part of a failed fixture start's message that tells a slow machine
/// from a slow or stuck fixture. Read it before the child is stopped.
pub(super) fn stall_evidence(child: &Child) -> String {
    let tree = describe(&tree(child.id()));
    let memory = describe_memory(os::memory());
    format!(
        "process tree of the fixture:\n{tree}\n{}\n{memory}",
        trivial_start()
    )
}

/// `free of total MB`, from bytes, or that the OS does not say.
fn describe_memory(memory: Option<(u64, u64)>) -> String {
    memory.map_or_else(
        || "free memory: unknown on this OS".to_owned(),
        |(free, total)| format!("free memory: {} of {} MB", free >> 20, total >> 20),
    )
}

/// The tree under `child`, one line per process, for a start that was slow but made it.
pub(super) fn tree_summary(child: &Child) -> String {
    tree(child.id())
        .iter()
        .map(|p| {
            let cpu = p
                .cpu
                .map_or_else(|| "?".to_owned(), |c| format!("{:.2}s", c.as_secs_f64()));
            format!("{}/{} {cpu}", p.name, p.pid)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// User plus system CPU time of a `/proc/<pid>/stat` line, at the 100 Hz tick every Linux target
/// uses. The process name (field 2) is in parentheses and may hold spaces and parentheses, so the
/// fields are counted from the last `)`.
#[cfg_attr(
    not(any(target_os = "linux", test)),
    expect(
        dead_code,
        reason = "only Linux has /proc; its parser is tested everywhere"
    )
)]
fn parse_cpu_time(stat: &str) -> Option<Duration> {
    let mut fields = stat[stat.rfind(')')? + 1..].split_whitespace().skip(11);
    let utime: u64 = fields.next()?.parse().ok()?;
    let stime: u64 = fields.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
}

#[cfg(target_os = "linux")]
mod os {
    use std::time::{Duration, SystemTime};

    use super::{Proc, parse_cpu_time};

    /// The boot time from `/proc/stat` (`btime`, seconds since the epoch).
    fn boot() -> Option<SystemTime> {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        let secs = stat
            .lines()
            .find_map(|l| l.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
    }

    /// One `/proc/<pid>/stat` line as a [`Proc`].
    pub(super) fn parse(stat: &str, boot: Option<SystemTime>) -> Option<Proc> {
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        let pid = stat.get(..open)?.trim().parse().ok()?;
        let name = stat.get(open + 1..close)?.to_owned();
        let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
        let parent = fields.get(1)?.parse().ok()?;
        let ticks: Option<u64> = fields.get(19).and_then(|t| t.parse().ok());
        let created = boot
            .zip(ticks)
            .map(|(b, t)| b + Duration::from_millis(t * 10));
        Some(Proc {
            pid,
            parent,
            name,
            created,
            cpu: parse_cpu_time(stat),
        })
    }

    /// Available and total memory in bytes, from `/proc/meminfo`.
    pub(super) fn memory() -> Option<(u64, u64)> {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let field = |name: &str| -> Option<u64> {
            let kib: u64 = info
                .lines()
                .find_map(|l| l.strip_prefix(name))?
                .trim()
                .trim_end_matches("kB")
                .trim()
                .parse()
                .ok()?;
            Some(kib * 1024)
        };
        Some((field("MemAvailable:")?, field("MemTotal:")?))
    }

    pub(super) fn snapshot() -> Vec<Proc> {
        let boot = boot();
        std::fs::read_dir("/proc")
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .bytes()
                    .all(|b| b.is_ascii_digit())
            })
            .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
            .filter_map(|stat| parse(&stat, boot))
            .collect()
    }
}

#[cfg(windows)]
mod os {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::time::{Duration, SystemTime};

    use windows_sys::Win32::Foundation::{FILETIME, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::Proc;

    /// A `FILETIME` count of 100 ns intervals.
    fn ticks(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }

    /// 100 ns intervals from 1601-01-01 (the `FILETIME` epoch) to 1970-01-01.
    const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;

    /// When `pid` was created and its user plus kernel time, or `None` for a process this user
    /// may not query or that has ended.
    #[expect(
        unsafe_code,
        reason = "Win32 FFI: std has no call that reads another process's CPU times"
    )]
    fn times(pid: u32) -> Option<(SystemTime, Duration)> {
        // SAFETY: OpenProcess takes plain values and returns a new handle or null.
        let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if raw.is_null() {
            return None;
        }
        // SAFETY: `raw` is a process handle OpenProcess just returned to us and nothing else
        // owns; the OwnedHandle closes it once.
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut created = FILETIME::default();
        let mut exited = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: per Microsoft Learn (GetProcessTimes), the handle has
        // PROCESS_QUERY_LIMITED_INFORMATION access and each out pointer is a writable FILETIME.
        // https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getprocesstimes
        let ok = unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &raw mut created,
                &raw mut exited,
                &raw mut kernel,
                &raw mut user,
            )
        };
        if ok == 0 {
            return None;
        }
        let since_epoch = ticks(created).checked_sub(UNIX_EPOCH_TICKS)?;
        let created = SystemTime::UNIX_EPOCH + Duration::from_nanos(since_epoch * 100);
        let cpu = Duration::from_nanos((ticks(kernel) + ticks(user)) * 100);
        Some((created, cpu))
    }

    /// Available and total physical memory in bytes.
    #[expect(
        unsafe_code,
        reason = "Win32 FFI: std has no call that reads the machine's memory status"
    )]
    pub(super) fn memory() -> Option<(u64, u64)> {
        let mut status = MEMORYSTATUSEX {
            dwLength: u32::try_from(size_of::<MEMORYSTATUSEX>()).ok()?,
            dwMemoryLoad: 0,
            ullTotalPhys: 0,
            ullAvailPhys: 0,
            ullTotalPageFile: 0,
            ullAvailPageFile: 0,
            ullTotalVirtual: 0,
            ullAvailVirtual: 0,
            ullAvailExtendedVirtual: 0,
        };
        // SAFETY: per Microsoft Learn (GlobalMemoryStatusEx), a writable MEMORYSTATUSEX whose
        // dwLength is its own size.
        // https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-globalmemorystatusex
        let ok = unsafe { GlobalMemoryStatusEx(&raw mut status) };
        (ok != 0).then_some((status.ullAvailPhys, status.ullTotalPhys))
    }

    /// Every process of the machine (pid, parent, executable name), with the times of those this
    /// user may query.
    #[expect(
        unsafe_code,
        reason = "Win32 FFI: std has no call that lists processes"
    )]
    pub(super) fn snapshot() -> Vec<Proc> {
        // SAFETY: CreateToolhelp32Snapshot takes plain values and returns a new handle or
        // INVALID_HANDLE_VALUE.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        // SAFETY: `raw` is a snapshot handle CreateToolhelp32Snapshot just returned to us and
        // nothing else owns; the OwnedHandle closes it once.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(u32::MAX),
            ..PROCESSENTRY32W::default()
        };
        let mut all = Vec::new();
        // SAFETY: per Microsoft Learn (Process32FirstW), a process snapshot handle and an entry
        // whose dwSize is its own size.
        // https://learn.microsoft.com/en-us/windows/win32/api/tlhelp32/nf-tlhelp32-process32firstw
        let mut more = unsafe { Process32FirstW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(entry.szExeFile.get(..len).unwrap_or_default());
            let times = times(entry.th32ProcessID);
            all.push(Proc {
                pid: entry.th32ProcessID,
                parent: entry.th32ParentProcessID,
                name,
                created: times.map(|t| t.0),
                cpu: times.map(|t| t.1),
            });
            // SAFETY: as for Process32FirstW: the same snapshot and entry.
            more = unsafe { Process32NextW(snapshot.as_raw_handle(), &raw mut entry) } != 0;
        }
        all
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod os {
    use super::Proc;

    pub(super) fn snapshot() -> Vec<Proc> {
        Vec::new()
    }

    pub(super) fn memory() -> Option<(u64, u64)> {
        None
    }
}

#[cfg(test)]
mod tests;
