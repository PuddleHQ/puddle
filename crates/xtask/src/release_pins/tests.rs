// SPDX-License-Identifier: AGPL-3.0-or-later
use std::ffi::OsString;

use super::*;

const H: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn windows_pins(tag: &str) -> String {
    format!("{tag} msb-windows-x86_64.exe {H}\n{tag} libkrunfw-windows-x86_64.dll {H}\n")
}

fn linux_pins(tag: &str) -> String {
    format!(
        "{tag} msb-linux-x86_64 {H}\n{tag} libkrunfw-linux-x86_64.so {H}\n{tag} libkrunfw-linux-version 5.6.1\n"
    )
}

#[test]
fn a_fully_pinned_tag_passes_for_the_oses_it_is_pinned_for() {
    let pins = windows_pins("v1.0.0-puddle.1") + &linux_pins("v1.0.0-puddle.1");
    for oses in [vec!["windows"], vec!["linux"], vec!["windows", "linux"]] {
        let line = check(&pins, "1.0.0-puddle.1", &oses).unwrap();
        assert!(
            line.contains("v1.0.0-puddle.1") && line.contains(&oses.join(", ")),
            "{line}"
        );
    }
}

#[test]
fn a_tag_with_no_lines_is_refused_naming_both_files() {
    let pins = windows_pins("v1.0.0-puddle.1");
    let err = check(&pins, "1.0.0-puddle.2", &["windows"]).unwrap_err();
    assert!(matches!(err, XtaskError::Unpinned(_)), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("v1.0.0-puddle.2 has no msb hash for windows"),
        "{text}"
    );
    assert!(
        text.contains("v1.0.0-puddle.2 has no firmware hash for windows"),
        "{text}"
    );
}

#[test]
fn one_missing_hash_of_two_is_refused() {
    let pins = format!("v1 msb-windows-x86_64.exe {H}\n");
    let text = check(&pins, "1", &["windows"]).unwrap_err().to_string();
    assert!(
        text.contains("no firmware hash") && !text.contains("no msb hash"),
        "{text}"
    );
}

#[test]
fn a_linux_tag_built_from_source_has_no_msb_hash_and_is_refused() {
    // The shape of the older fork tags: a source commit for msb, firmware pinned by version only.
    let pins = "v1 source-commit abc\nv1 libkrunfw-linux-version 5.6.1\n";
    let text = check(pins, "1", &["linux"]).unwrap_err().to_string();
    assert!(text.contains("no msb hash for linux"), "{text}");
    assert!(!text.contains("firmware version"), "{text}");
}

#[test]
fn a_linux_tag_without_a_firmware_version_is_refused() {
    let pins = format!("v1 msb-linux-x86_64 {H}\nv1 libkrunfw-linux-x86_64.so {H}\n");
    let text = check(&pins, "1", &["linux"]).unwrap_err().to_string();
    assert!(text.contains("no firmware version for linux"), "{text}");
}

#[test]
fn every_os_is_reported_not_only_the_first() {
    let text = check("", "1", &["windows", "linux"])
        .unwrap_err()
        .to_string();
    assert_eq!(text.matches("for windows").count(), 2, "{text}");
    assert_eq!(text.matches("for linux").count(), 3, "{text}");
}

#[test]
fn a_value_that_is_not_a_hash_is_a_parse_error_not_a_pass() {
    let pins = "v1 msb-windows-x86_64.exe zz\nv1 libkrunfw-windows-x86_64.dll zz\n";
    let err = check(pins, "1", &["windows"]).unwrap_err();
    assert!(matches!(err, XtaskError::Parse { .. }), "{err}");
}

#[test]
fn the_committed_list_passes_for_the_tag_this_build_is_made_for() {
    // The release's `verify` job runs this check; a pin bump that forgets the hashes fails here.
    let args = ["release-pins", "--os", "windows"].map(OsString::from);
    let line = crate::run(args).unwrap();
    assert!(line.starts_with("release pins ok"), "{line}");
}

#[test]
fn the_command_reports_a_tag_with_no_pins_and_a_bad_os() {
    let run = |list: &[&str]| crate::run(list.iter().map(OsString::from));
    let err = run(&["release-pins", "--tag", "v0.0.0-puddle.0"]).unwrap_err();
    assert!(matches!(err, XtaskError::Unpinned(_)), "{err}");
    let err = run(&["release-pins", "--os", "macos"]).unwrap_err();
    assert!(matches!(err, XtaskError::Usage(_)), "{err}");
}
