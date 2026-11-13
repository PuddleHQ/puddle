// SPDX-License-Identifier: AGPL-3.0-or-later
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
