// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_header_name_is_lower_case_for_the_proxy_comparison() {
    assert_eq!(PROGRAM_HEADER, PROGRAM_HEADER.to_ascii_lowercase());
}

#[test]
fn a_plain_tool_name_is_kept() {
    for name in [
        "pip",
        "node",
        "curl",
        "python3.12",
        "g++",
        "cargo-nextest",
        "a_b",
    ] {
        assert_eq!(Program::sanitise(name).unwrap().as_str(), name);
    }
}

#[test]
fn line_breaks_and_other_characters_are_dropped() {
    let p = Program::sanitise("cu\r\nrl: x\0 /bin/\u{202e}sh").unwrap();
    assert_eq!(p.as_str(), "curlxbinsh");
}

#[test]
fn a_name_is_cut_to_the_cap() {
    let p = Program::sanitise(&"a".repeat(100)).unwrap();
    assert_eq!(p.as_str().len(), PROGRAM_MAX_CHARS);
}

#[test]
fn a_name_with_nothing_allowed_is_no_name() {
    assert_eq!(Program::sanitise(""), None);
    assert_eq!(Program::sanitise(" \r\n: /"), None);
}

#[test]
fn display_prints_the_name() {
    assert_eq!(Program::sanitise("pip").unwrap().to_string(), "pip");
}
