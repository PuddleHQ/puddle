// SPDX-License-Identifier: AGPL-3.0-or-later
//! The build script's choice of expected hashes (`build/digests.rs`): which pin lines a target
//! reads, and what makes the build fail.

#[path = "../build/digests.rs"]
mod digests;
#[path = "../build/pin_list.rs"]
mod pin_list;

use digests::{Expected, expected};

const MSB: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const DLL: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const LINUX_MSB: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const SO: &str = "4444444444444444444444444444444444444444444444444444444444444444";
const FORK_SO: &str = "5555555555555555555555555555555555555555555555555555555555555555";

fn pins() -> String {
    format!(
        "# comment\n\
         v1.2.3-puddle.4 msb-windows-x86_64.exe {MSB}\n\
         v1.2.3-puddle.4 libkrunfw-windows-x86_64.dll {DLL}\n\
         v1.2.3-puddle.4 msb-linux-x86_64 {LINUX_MSB}\n\
         v1.2.3-puddle.4 libkrunfw-linux-x86_64.so {SO}\n\
         v1.2.3-puddle.4 libkrunfw-linux-version 5.6.1\n\
         v1.2.3-puddle.5 msb-linux-x86_64 {LINUX_MSB}\n\
         v1.2.3-puddle.5 libkrunfw-linux-tag v9-fw\n\
         v1.2.3-puddle.5 libkrunfw-linux-version 5.6.1\n\
         v9-fw libkrunfw-linux-x86_64.so {FORK_SO}\n"
    )
}

#[test]
fn windows_reads_the_tag_s_msb_and_dll() {
    let got = expected(&pins(), "1.2.3-puddle.4", "windows", "x86_64").unwrap();
    assert_eq!(
        got,
        Expected {
            msb: Some(MSB.into()),
            firmware: Some(DLL.into()),
            firmware_version: None,
        }
    );
}

#[test]
fn linux_reads_the_firmware_of_the_tag_unless_it_pins_a_fork_release() {
    let own = expected(&pins(), "1.2.3-puddle.4", "linux", "x86_64").unwrap();
    assert_eq!(own.msb.as_deref(), Some(LINUX_MSB));
    assert_eq!(own.firmware.as_deref(), Some(SO));
    assert_eq!(own.firmware_version.as_deref(), Some("5.6.1"));
    let fork = expected(&pins(), "1.2.3-puddle.5", "linux", "x86_64").unwrap();
    assert_eq!(fork.firmware.as_deref(), Some(FORK_SO));
}

#[test]
fn a_target_the_list_does_not_cover_expects_nothing() {
    for (os, arch) in [
        ("macos", "aarch64"),
        ("linux", "aarch64"),
        ("windows", "x86"),
    ] {
        assert_eq!(
            expected(&pins(), "1.2.3-puddle.4", os, arch).unwrap(),
            Expected::default(),
            "{os} {arch}"
        );
    }
}

#[test]
fn a_tag_without_lines_expects_nothing() {
    assert_eq!(
        expected(&pins(), "9.9.9-puddle.1", "windows", "x86_64").unwrap(),
        Expected::default()
    );
}

#[test]
fn a_value_that_is_not_a_sha256_fails_the_build() {
    for bad in ["abc", &MSB.replace('1', "g"), &format!("{MSB}00")] {
        let pins = format!("v1.2.3-puddle.4 msb-windows-x86_64.exe {bad}\n");
        let err = expected(&pins, "1.2.3-puddle.4", "windows", "x86_64").unwrap_err();
        assert!(err.contains("not a SHA-256") && err.contains(bad), "{err}");
    }
}

#[test]
fn hashes_are_embedded_in_lower_case() {
    let pins = format!(
        "v1.2.3-puddle.4 msb-windows-x86_64.exe {}\n",
        MSB.replace('1', "A")
    );
    let got = expected(&pins, "1.2.3-puddle.4", "windows", "x86_64").unwrap();
    assert_eq!(got.msb, Some(MSB.replace('1', "a")));
}

#[test]
fn a_firmware_version_that_is_not_a_dotted_number_fails_the_build() {
    let pins = "v1.2.3-puddle.4 libkrunfw-linux-version 5.6/../1\n";
    let err = expected(pins, "1.2.3-puddle.4", "linux", "x86_64").unwrap_err();
    assert!(err.contains("not a dotted number"), "{err}");
}
