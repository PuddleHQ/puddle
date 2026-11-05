// SPDX-License-Identifier: GPL-3.0-or-later
//! The host-side resident memory of a process, in bytes: the number a VM test reads off the
//! process that runs a microVM to see whether memory the guest freed went back to the host.
//!
//! Linux reads `VmRSS` from `/proc/<pid>/status`; Windows asks `tasklist` for the working set,
//! because the harness forbids `unsafe` and `GetProcessMemoryInfo` needs it; other Unix systems
//! ask `ps`.

use crate::HarnessError;

/// The resident set of process `pid` in bytes.
///
/// # Errors
///
/// [`HarnessError::Io`] when the process is gone or its figure can't be read or parsed.
pub fn resident_bytes(pid: u32) -> Result<u64, HarnessError> {
    read(pid).map_err(|e| HarnessError::io("read resident memory of", format!("pid {pid}"), e))
}

fn invalid(what: &str, text: &str) -> std::io::Error {
    let shown: String = text.chars().take(120).collect();
    std::io::Error::other(format!("{what}: {shown:?}"))
}

#[cfg(target_os = "linux")]
fn read(pid: u32) -> std::io::Result<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    parse_vm_rss(&status)
}

#[cfg(windows)]
fn read(pid: u32) -> std::io::Result<u64> {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()?;
    parse_tasklist(&String::from_utf8_lossy(&out.stdout))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read(pid: u32) -> std::io::Result<u64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()?;
    parse_kib(String::from_utf8_lossy(&out.stdout).trim())
}

/// A figure in KiB with an optional `kB`/`K` unit and thousands separators.
fn parse_kib(text: &str) -> std::io::Result<u64> {
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let kib: u64 = digits
        .parse()
        .map_err(|_| invalid("no memory figure in", text))?;
    Ok(kib * 1024)
}

#[cfg_attr(
    not(any(test, target_os = "linux")),
    expect(dead_code, reason = "only Linux reads /proc/<pid>/status")
)]
fn parse_vm_rss(status: &str) -> std::io::Result<u64> {
    let line = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .ok_or_else(|| invalid("no VmRSS line in", status))?;
    parse_kib(line.trim())
}

/// A CSV row of quoted fields whose last one is the working set (`"123,456 K"`): that field. A missing process prints an
/// `INFO:` line instead, which has no quoted fields.
#[cfg_attr(
    not(any(test, windows)),
    expect(dead_code, reason = "only Windows asks tasklist")
)]
fn parse_tasklist(csv: &str) -> std::io::Result<u64> {
    let line = csv
        .lines()
        .find(|l| l.starts_with('"'))
        .ok_or_else(|| invalid("no process in", csv))?;
    let field = line
        .trim_end()
        .trim_end_matches('"')
        .rsplit('"')
        .next()
        .unwrap_or_default();
    parse_kib(field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_status_gives_the_rss_line_in_bytes() {
        let status = "Name:\tmsb\nVmHWM:\t  900 kB\nVmRSS:\t  123456 kB\nThreads:\t4\n";
        assert_eq!(parse_vm_rss(status).unwrap(), 123_456 * 1024);
    }

    #[test]
    fn a_status_without_rss_is_an_error_that_shows_the_text() {
        let err = parse_vm_rss("Name:\tkthread\n").unwrap_err().to_string();
        assert!(
            err.contains("no VmRSS line") && err.contains("kthread"),
            "{err}"
        );
    }

    #[test]
    fn tasklist_csv_gives_the_working_set_in_bytes() {
        let csv = "\"msb.exe\",\"1234\",\"Console\",\"1\",\"123,456 K\"\r\n";
        assert_eq!(parse_tasklist(csv).unwrap(), 123_456 * 1024);
    }

    #[test]
    fn tasklist_with_no_match_is_an_error() {
        let info = "INFO: No tasks are running which match the specified criteria.\r\n";
        assert!(parse_tasklist(info).is_err());
    }

    #[test]
    #[cfg(any(target_os = "linux", windows))]
    fn this_process_has_resident_memory() {
        let bytes = resident_bytes(std::process::id()).unwrap();
        assert!(bytes > 1024 * 1024, "{bytes}");
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn a_process_that_is_gone_is_an_error() {
        // The kernel's pid limit is 2^22, so this id is never a live process.
        assert!(resident_bytes(u32::MAX - 1).is_err());
    }
}
