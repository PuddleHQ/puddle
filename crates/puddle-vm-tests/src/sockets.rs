// SPDX-License-Identifier: GPL-3.0-or-later
//! The machine's listening sockets and process tree, for the hostile-guest sweep that asks
//! "does Puddle, or the runtime it started, listen on anything but loopback?".
//!
//! The parsers take the text of `netstat -ano` and of a process listing, so they run (and are
//! tested) on every OS; `listening_sockets` and `process_table` call the Windows tools.

use std::collections::BTreeSet;
use std::net::IpAddr;

/// A TCP or UDP socket that waits for traffic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Listening {
    /// `TCP` or `UDP`.
    pub protocol: &'static str,
    /// The local address the socket is bound to (`0.0.0.0` and `::` mean every interface).
    pub address: IpAddr,
    /// The local port.
    pub port: u16,
    /// The owning process.
    pub pid: u32,
}

impl Listening {
    /// Whether only this machine can reach the socket.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.address.is_loopback()
    }
}

impl std::fmt::Display for Listening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}:{} (pid {})",
            self.protocol, self.address, self.port, self.pid
        )
    }
}

/// The listening TCP sockets and the bound UDP sockets in the output of `netstat -ano`.
///
/// A TCP row counts when its state is `LISTENING`; a UDP row has no state and counts always.
/// Header lines and other protocols are skipped.
///
/// # Errors
///
/// A TCP or UDP row whose address, port or process id can't be read: a sweep must never pass
/// because it couldn't read a row.
pub fn parse_netstat(text: &str) -> Result<Vec<Listening>, String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (protocol, local, pid) = match fields.as_slice() {
            ["TCP", local, _foreign, "LISTENING", pid] => ("TCP", *local, *pid),
            ["TCP", _local, _foreign, _state, _pid] => continue,
            ["UDP", local, _foreign, pid] => ("UDP", *local, *pid),
            _ => continue,
        };
        let bad = |what: &str| format!("netstat row {line:?}: bad {what}");
        let (address, port) = local.rsplit_once(':').ok_or_else(|| bad("address"))?;
        // `[fe80::1%12]` carries the interface index after a percent sign.
        let address = address.trim_start_matches('[').trim_end_matches(']');
        let address = address.split('%').next().unwrap_or(address);
        found.push(Listening {
            protocol,
            address: address.parse().map_err(|_| bad("address"))?,
            port: port.parse().map_err(|_| bad("port"))?,
            pid: pid.parse().map_err(|_| bad("process id"))?,
        });
    }
    Ok(found)
}

/// One row of the process table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    /// The process id.
    pub pid: u32,
    /// The id of the process that started it.
    pub parent: u32,
    /// The image name (`msb.exe`).
    pub name: String,
}

/// Rows of `<pid> <parent pid> <image name>` per line.
///
/// # Errors
///
/// A row that isn't of that form.
pub fn parse_processes(text: &str) -> Result<Vec<Process>, String> {
    let mut rows = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let mut parts = line.splitn(3, ' ');
        let row = (|| {
            Some(Process {
                pid: parts.next()?.parse().ok()?,
                parent: parts.next()?.parse().ok()?,
                name: parts.next()?.trim().to_owned(),
            })
        })();
        rows.push(row.ok_or_else(|| format!("process row {line:?} is not `pid parent name`"))?);
    }
    Ok(rows)
}

/// `root` and every process that descends from it, by parent id.
///
/// Windows reuses ids, so a stale parent id can add a stranger; for the sweep, too many processes
/// only means more sockets looked at.
#[must_use]
pub fn descendants(table: &[Process], root: u32) -> BTreeSet<u32> {
    let mut family = BTreeSet::from([root]);
    loop {
        let before = family.len();
        for process in table {
            if family.contains(&process.parent) {
                family.insert(process.pid);
            }
        }
        if family.len() == before {
            return family;
        }
    }
}

/// Every listening socket on the machine (`netstat -ano`).
///
/// # Errors
///
/// `netstat` could not run, or a row was unreadable.
#[cfg(windows)]
pub fn listening_sockets() -> Result<Vec<Listening>, String> {
    let out = std::process::Command::new("netstat")
        .arg("-ano")
        .output()
        .map_err(|e| format!("cannot run netstat: {e}"))?;
    if !out.status.success() {
        return Err(format!("netstat failed: {}", out.status));
    }
    parse_netstat(&String::from_utf8_lossy(&out.stdout))
}

/// The machine's process table (PowerShell, `Win32_Process`).
///
/// # Errors
///
/// PowerShell could not run, or a row was unreadable.
#[cfg(windows)]
pub fn process_table() -> Result<Vec<Process>, String> {
    let script = "Get-CimInstance Win32_Process | ForEach-Object { \"$($_.ProcessId) $($_.ParentProcessId) $($_.Name)\" }";
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| format!("cannot run powershell: {e}"))?;
    if !out.status.success() {
        return Err(format!("powershell failed: {}", out.status));
    }
    parse_processes(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NETSTAT: &str = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1032
  TCP    127.0.0.1:49152        0.0.0.0:0              LISTENING       4100
  TCP    127.0.0.1:49152        127.0.0.1:50000        ESTABLISHED     4100
  TCP    [::]:445               [::]:0                 LISTENING       4
  TCP    [::1]:8080             [::]:0                 LISTENING       4100
  UDP    0.0.0.0:123            *:*                                    2344
  UDP    [fe80::1%12]:1900      *:*                                    2344
  UDP    127.0.0.1:5353         *:*                                    4100
";

    #[test]
    fn listening_and_bound_sockets_are_read_and_established_ones_are_not() {
        let rows = parse_netstat(NETSTAT).unwrap();
        let shown: Vec<String> = rows.iter().map(ToString::to_string).collect();
        assert_eq!(
            shown,
            [
                "TCP 0.0.0.0:135 (pid 1032)",
                "TCP 127.0.0.1:49152 (pid 4100)",
                "TCP :::445 (pid 4)",
                "TCP ::1:8080 (pid 4100)",
                "UDP 0.0.0.0:123 (pid 2344)",
                "UDP fe80::1:1900 (pid 2344)",
                "UDP 127.0.0.1:5353 (pid 4100)",
            ]
        );
        let open: Vec<bool> = rows.iter().map(|r| !r.is_loopback()).collect();
        assert_eq!(open, [true, false, true, false, true, true, false]);
    }

    #[test]
    fn a_row_that_cannot_be_read_fails_the_sweep() {
        for bad in [
            "  TCP    nonsense               0.0.0.0:0              LISTENING       1",
            "  TCP    0.0.0.0:99999          0.0.0.0:0              LISTENING       1",
            "  TCP    0.0.0.0:80             0.0.0.0:0              LISTENING       x",
            "  UDP    0.0.0.0:80             *:*                                    -1",
            "  UDP    host:80                *:*                                    1",
        ] {
            assert!(parse_netstat(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn processes_are_read_and_their_descendants_found() {
        let table = parse_processes("10 1 test.exe\n11 10 msb.exe\n12 11 msb helper.exe\n\n20 1 other.exe\n13 12 deep.exe\n")
            .unwrap();
        assert_eq!(table.len(), 5);
        assert_eq!(table[2].name, "msb helper.exe");
        assert_eq!(descendants(&table, 10), BTreeSet::from([10, 11, 12, 13]));
        assert_eq!(descendants(&table, 20), BTreeSet::from([20]));
        assert_eq!(descendants(&table, 99), BTreeSet::from([99]));
    }

    #[test]
    fn a_process_row_that_cannot_be_read_is_an_error() {
        for bad in ["x 1 a.exe", "1 x a.exe", "1 2", "1"] {
            assert!(parse_processes(bad).is_err(), "{bad}");
        }
    }
}
