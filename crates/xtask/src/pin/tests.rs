// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const PINS: &str = "# comment v1 libkrunfw-linux-version 9\n\
        v1 msb-windows-x86_64.exe aaaa\n\
        v1 libkrunfw-linux-version 5.6.1\n\
        v2\tlibkrunfw-linux-version   5.7.0  \n\
        v3 libkrunfw-linux-version 5.7/../x\n\
        v4 libkrunfw-linux-version\n\
        v5 libkrunfw-linux-version 5..1\n\
        v1 libkrunfw-linux-tag v5.6.1-puddle.1\n";

#[test]
fn reads_the_firmware_fork_release_of_a_tag() {
    assert_eq!(
        libkrunfw_linux_tag(PINS, "v1").as_deref(),
        Some("v5.6.1-puddle.1")
    );
    assert_eq!(libkrunfw_linux_tag(PINS, "v2"), None);
}

#[test]
fn reads_the_value_of_the_matching_line() {
    assert_eq!(pinned(PINS, "v1", "msb-windows-x86_64.exe"), Some("aaaa"));
    assert_eq!(pinned(PINS, "v2", LIBKRUNFW_LINUX_VERSION), Some("5.7.0"));
    assert_eq!(pinned(PINS, "v9", LIBKRUNFW_LINUX_VERSION), None);
    assert_eq!(pinned(PINS, "v1", LIBKRUNFW_LINUX_VERSION), Some("5.6.1"));
    assert_eq!(pinned(PINS, "v4", LIBKRUNFW_LINUX_VERSION), None);
}

#[test]
fn a_pin_bump_changes_the_version() {
    assert_eq!(libkrunfw_linux_version(PINS, "v1").unwrap(), "5.6.1");
    assert_eq!(libkrunfw_linux_version(PINS, "v2").unwrap(), "5.7.0");
}

#[test]
fn a_missing_or_unsafe_version_is_refused() {
    for tag in ["v9", "v3", "v4", "v5"] {
        let err = libkrunfw_linux_version(PINS, tag).unwrap_err().to_string();
        assert!(err.contains(PIN_FILE) && err.contains(tag), "{err}");
    }
}

#[test]
#[cfg(unix)]
fn agrees_with_fetch_msb_on_every_pin_line() {
    let root = crate::workspace_root();
    let pins = std::fs::read_to_string(root.join(PIN_FILE)).unwrap();
    let script = root.join("ci/fetch-msb.sh");
    let shell = |tag: &str, name: &str| {
        let out = std::process::Command::new("sh")
            .arg(&script)
            .args(["--pinned", tag, name])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap().trim_end().to_owned())
    };
    let mut seen = 0;
    let pairs = pins.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        Some((fields.next().filter(|t| *t != "#")?, fields.next()?))
    });
    for (tag, name) in pairs {
        assert_eq!(
            shell(tag, name).as_deref(),
            pinned(&pins, tag, name),
            "{tag} {name}"
        );
        seen += 1;
    }
    assert!(seen > 10, "the pin list has {seen} lines?");
    assert_eq!(shell("v0.0.0-nope", LIBKRUNFW_LINUX_VERSION), None);
    assert_eq!(pinned(&pins, "v0.0.0-nope", LIBKRUNFW_LINUX_VERSION), None);
}

const SUM: &str = "ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12ab12";
const OTHER: &str = "cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34cd34";

#[test]
fn a_download_matching_its_pin_passes_whatever_the_case() {
    let pins = format!("# c\nv1 msb {}\n", SUM.to_ascii_uppercase());
    verify(&pins, "v1", "msb", SUM).unwrap();
}

#[test]
fn a_download_with_another_hash_is_refused_naming_file_list_and_both_hashes() {
    let pins = format!("v1 msb {SUM}\n");
    let err = verify(&pins, "v1", "msb", OTHER).unwrap_err().to_string();
    assert!(
        err.contains("checksum mismatch for msb")
            && err.contains("ci/msb-runtime.sha256 (v1)")
            && err.contains(SUM)
            && err.contains(OTHER),
        "{err}"
    );
}

#[test]
fn a_download_nobody_pinned_is_refused_naming_it() {
    // Another tag's line for the same file must not count.
    let pins = format!("v2 msb {SUM}\nv1 other {SUM}\n");
    let err = verify(&pins, "v1", "msb", SUM).unwrap_err().to_string();
    assert!(
        err.contains("msb has no entry in ci/msb-runtime.sha256 (v1)"),
        "{err}"
    );
}
