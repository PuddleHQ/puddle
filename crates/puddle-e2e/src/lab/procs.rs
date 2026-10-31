// SPDX-License-Identifier: GPL-3.0-or-later
//! What a fixture's processes are doing: the tree under the process a fixture started, with each
//! process's start time and CPU time, and how long the machine takes to start a trivial program.
//!
//! A fixture start that misses its window reports these, so its failure says which of three it
//! was: the program had not been started yet (a later process of the tree is missing or started
//! late), it was started and worked (CPU time), or it was started and waited (no CPU time); and
//! whether starting any program was slow at that moment.

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
    if cfg!(windows) {
        time_to_end("cmd", &["/c", "exit 0"], TRIVIAL_START_BOUND)
    } else {
        time_to_end("true", &[], TRIVIAL_START_BOUND)
    }
}

/// Runs `program` and says how long it took from the call to its end, or that it had not ended
/// within `bound` (it is then stopped).
fn time_to_end(program: &str, args: &[&str], bound: Duration) -> String {
    let what = format!("{program} {}", args.join(" "));
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
    format!("process tree of the fixture:\n{tree}\n{}", trivial_start())
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
pub(super) fn parse_cpu_time(stat: &str) -> Option<Duration> {
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

    pub(super) fn snapshot() -> Vec<Proc> {
        let boot = boot();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        dir.filter_map(Result::ok)
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
    #[expect(unsafe_code, reason = "OpenProcess and GetProcessTimes")]
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

    /// Every process of the machine (pid, parent, executable name), with the times of those this
    /// user may query.
    #[expect(unsafe_code, reason = "the Toolhelp process snapshot")]
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: u32, created_s: u64) -> Proc {
        Proc {
            pid,
            parent,
            name: format!("p{pid}"),
            created: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(created_s)),
            cpu: Some(Duration::from_millis(u64::from(pid))),
        }
    }

    #[test]
    fn the_tree_holds_the_root_its_children_and_theirs_and_no_one_else() {
        let all = [
            proc(1, 0, 0),
            proc(10, 1, 100),
            proc(11, 10, 101),
            proc(12, 11, 102),
            proc(20, 1, 100),
            proc(13, 10, 103),
        ];
        let pids: Vec<u32> = descendants(&all, 10).iter().map(|p| p.pid).collect();
        assert_eq!(pids, [10, 11, 13, 12]);
        assert_eq!(descendants(&all, 99).len(), 0);
    }

    #[test]
    fn a_process_older_than_its_parent_id_is_not_its_child() {
        // Process 30 was started by an earlier process 10 that has ended; the id was reused.
        let all = [proc(10, 1, 100), proc(30, 10, 50), proc(31, 10, 150)];
        let pids: Vec<u32> = descendants(&all, 10).iter().map(|p| p.pid).collect();
        assert_eq!(pids, [10, 31]);
    }

    #[test]
    fn the_description_gives_each_start_relative_to_the_first_and_its_cpu() {
        let mut late = proc(11, 10, 160);
        late.cpu = None;
        let text = describe(&[proc(10, 1, 100), late]);
        assert_eq!(
            text,
            "  p10 pid 10 started +0.00s CPU 0.01s\n  p11 pid 11 started +60.00s CPU unknown"
        );
        assert!(describe(&[]).contains("not readable"));
    }

    #[test]
    fn cpu_time_is_user_plus_system_ticks_counted_after_the_name() {
        // Field 2 holds a space and a parenthesis; utime and stime are fields 14 and 15.
        let line = "42 (mitm (dump) x) S 1 42 42 0 -1 4194560 100 0 0 0 55 17 0 0 20 0 1 0 100 0 0";
        assert_eq!(parse_cpu_time(line), Some(Duration::from_millis(720)));
        assert_eq!(parse_cpu_time("no name here"), None);
        assert_eq!(parse_cpu_time("1 (x) S 1"), None);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_proc_stat_line_gives_pid_parent_name_start_and_cpu() {
        let line = "42 (mitm (dump) x) S 7 42 42 0 -1 4194560 100 0 0 0 55 17 0 0 20 0 1 0 250 0 0";
        let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let p = os::parse(line, Some(boot)).unwrap();
        assert_eq!(
            p,
            Proc {
                pid: 42,
                parent: 7,
                name: "mitm (dump) x".to_owned(),
                created: Some(boot + Duration::from_millis(2500)),
                cpu: Some(Duration::from_millis(720)),
            }
        );
    }

    /// A shell that starts a long child and waits for it: a tree of two.
    fn shell_with_a_child() -> Child {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/c", "ping -n 30 127.0.0.1 >nul & exit 0"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 30; true"]);
            c
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The grandchild outlives a killed shell; it must hold none of the test's pipes.
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn a_running_tree_is_read_with_its_child_and_reported() {
        let mut shell = shell_with_a_child();
        let pid = shell.id();
        let found = puddle_test_kit::eventually_blocking(
            "the shell's child process",
            Duration::from_secs(30),
            || {
                let tree = tree(pid);
                (tree.len() >= 2).then_some(tree)
            },
        );
        let evidence = stall_evidence(&shell);
        let cpu = tree_cpu(pid);
        let summary = tree_summary(&shell);
        drop(shell.kill());
        drop(shell.wait());
        assert_eq!(found.first().map(|p| p.pid), Some(pid));
        let [_, child, ..] = found.as_slice() else {
            panic!("no child in {found:?}");
        };
        let expected = if cfg!(windows) { "PING.EXE" } else { "sleep" };
        assert!(child.name.eq_ignore_ascii_case(expected), "{found:?}");
        assert!(child.created.is_some() && child.cpu.is_some(), "{found:?}");
        assert!(cpu.is_some());
        assert!(summary.contains(&format!("/{pid} ")), "{summary}");
        assert!(
            evidence.contains(&format!("pid {pid} started +0.00s")),
            "{evidence}"
        );
        assert!(evidence.contains("a trivial program"), "{evidence}");
        assert!(evidence.contains("to start and end"), "{evidence}");
    }

    #[test]
    fn a_program_that_overruns_its_bound_is_reported() {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("ping", &["-n", "30", "127.0.0.1"])
        } else {
            ("sleep", &["30"])
        };
        let text = time_to_end(program, args, Duration::from_millis(200));
        assert!(text.contains("had not ended after 0.2s"), "{text}");
        assert!(trivial_start().contains("to start and end"));
        assert!(
            time_to_end("no-such-program-puddle", &[], TRIVIAL_START_BOUND)
                .contains("did not start")
        );
    }

    #[test]
    #[cfg(not(any(target_os = "linux", windows)))]
    fn without_a_process_table_there_is_no_tree() {
        assert!(tree(std::process::id()).is_empty());
        assert_eq!(tree_cpu(std::process::id()), None);
    }
}
