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

#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_host_reports_some_memory() {
    assert!(available_memory_bytes().unwrap() > 0);
}
