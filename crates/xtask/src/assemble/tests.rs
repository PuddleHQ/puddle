// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use puddle_runtime::RuntimeFiles;

#[test]
fn licence_texts_are_the_real_ones() {
    let [(_, apache), (_, lgpl), (_, gpl)] = LICENCE_TEXTS;
    assert!(apache.contains("Apache License") && apache.contains("Version 2.0, January 2004"));
    assert!(
        lgpl.contains("GNU LESSER GENERAL PUBLIC LICENSE")
            && lgpl.contains("Version 2.1, February 1999")
    );
    assert!(gpl.contains("GNU GENERAL PUBLIC LICENSE") && gpl.contains("Version 2, June 1991"));
}

const WIN: RuntimeFiles = HostOs::Windows.runtime_files();
const LINUX: RuntimeFiles = HostOs::Linux.runtime_files();

fn linux(version: &str) -> Target {
    Target::Linux {
        msb: None,
        libkrunfw_version: version.to_owned(),
        firmware_tag: None,
    }
}

fn firmware(fork_tag: Option<&str>) -> Firmware {
    Firmware {
        libkrunfw_version: "5.6.1".into(),
        kernel_version: "6.12.113".into(),
        repo_url: "https://github.com/PuddleHQ/libkrunfw".into(),
        commit: "e".repeat(40),
        fork_tag: fork_tag.map(ToOwned::to_owned),
    }
}

fn linux_from(fork_tag: Option<&str>) -> Target {
    Target::Linux {
        msb: None,
        libkrunfw_version: "5.6.1".into(),
        firmware_tag: fork_tag.map(ToOwned::to_owned),
    }
}

#[test]
fn the_firmware_comes_from_the_fork_release_the_pin_and_the_facts_both_name() {
    let pinned = Some("v5.6.1-puddle.1");
    assert_eq!(
        firmware_origin(&linux_from(pinned), &firmware(pinned)).unwrap(),
        Origin::FirmwareFork
    );
    assert_eq!(
        firmware_origin(&linux_from(None), &firmware(None)).unwrap(),
        Origin::Upstream
    );
    assert_eq!(
        firmware_origin(&Target::Windows, &firmware(None)).unwrap(),
        Origin::Upstream
    );
}

#[test]
fn a_pin_and_facts_naming_different_firmware_are_refused() {
    for (target, facts) in [
        (
            linux_from(Some("v5.6.1-puddle.2")),
            firmware(Some("v5.6.1-puddle.1")),
        ),
        (linux_from(Some("v5.6.1-puddle.1")), firmware(None)),
        (linux_from(None), firmware(Some("v5.6.1-puddle.1"))),
        (Target::Windows, firmware(Some("v5.6.1-puddle.1"))),
    ] {
        let err = firmware_origin(&target, &facts).unwrap_err().to_string();
        assert!(err.contains("v5.6.1-puddle.1"), "{err}");
    }
}

fn fw(name: &str) -> (String, Part) {
    (name.to_owned(), Part::Firmware)
}

#[test]
fn windows_ships_msb_and_one_firmware_file() {
    assert_eq!(
        plan(&Target::Windows).unwrap(),
        [(WIN.msb.to_owned(), Part::Msb), fw(WIN.libkrunfw)]
    );
}

#[test]
fn linux_ships_the_firmware_under_its_soname_and_the_pinned_version() {
    assert_eq!(
        plan(&linux("5.6.1")).unwrap(),
        [
            (LINUX.msb.to_owned(), Part::Msb),
            fw(LINUX.libkrunfw),
            fw(&format!("{}.6.1", LINUX.libkrunfw))
        ]
    );
    // A pin bump changes the name.
    assert_eq!(
        plan(&linux("5.7.0")).unwrap().last(),
        Some(&fw(&format!("{}.7.0", LINUX.libkrunfw)))
    );
}

#[test]
fn a_version_that_equals_the_soname_is_refused() {
    let err = plan(&linux("5")).unwrap_err().to_string();
    assert!(err.contains(LINUX.libkrunfw), "{err}");
}

#[test]
fn a_part_that_cannot_be_copied_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("xtask-assemble-no-such-msb");
    let err = place(
        &[("msb".to_owned(), Part::Msb)],
        &missing,
        &missing,
        dir.path(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("copying") && err.contains("no-such-msb"),
        "{err}"
    );
}

#[test]
fn staging_sits_beside_the_output() {
    assert_eq!(
        sibling(Path::new("/a/runtime"), "partial"),
        Path::new("/a/runtime.partial")
    );
}
