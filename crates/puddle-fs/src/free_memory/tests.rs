// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_figure_is_the_mem_available_line_in_bytes() {
    let text = "MemTotal:       16384000 kB\nMemFree:          100 kB\nMemAvailable:    2048 kB\n";
    assert_eq!(mem_available_bytes(text), Some(2048 * 1024));
}

#[test]
fn a_text_without_the_line_or_with_a_bad_figure_gives_nothing() {
    assert_eq!(mem_available_bytes("MemTotal: 1 kB\n"), None);
    assert_eq!(mem_available_bytes("MemAvailable: lots kB\n"), None);
    assert_eq!(mem_available_bytes("MemAvailable: 5\n"), None);
}

#[test]
fn a_figure_too_large_for_bytes_gives_nothing() {
    assert_eq!(mem_available_bytes("MemAvailable: 18446744073709551615 kB\n"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn the_host_reports_memory_between_a_megabyte_and_its_total() {
    let available = available_memory_bytes().unwrap();
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap();
    let total_kib: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .unwrap()
        .trim()
        .strip_suffix("kB")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(available > 1024 * 1024, "{available}");
    assert!(available <= total_kib * 1024, "{available}");
}

#[cfg(windows)]
#[test]
fn the_host_reports_some_commit() {
    assert!(available_memory_bytes().unwrap() > 1024 * 1024);
}
