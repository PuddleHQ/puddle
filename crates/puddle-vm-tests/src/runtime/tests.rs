// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::env::tests::TempDir;

/// The file names of `os`'s runtime: msb, and the firmware's prefix.
fn names(os: HostOs) -> (&'static str, &'static str) {
    let files = os.runtime_files();
    (files.msb, files.libkrunfw_prefix)
}

fn touch(dir: &Path, name: &str) {
    std::fs::write(dir.join(name), b"x").unwrap();
}

#[test]
fn ambient_vars_are_refused_by_name() {
    assert!(refuse_ambient_msb_vars(|_| None).is_ok());
    let err = refuse_ambient_msb_vars(|var| (var == "MSB_HOME").then(String::new)).unwrap_err();
    assert!(matches!(
        err,
        HarnessError::AmbientMsbVar { var: "MSB_HOME" }
    ));
    for var in AMBIENT_MSB_VARS {
        let err = refuse_ambient_msb_vars(|v| (v == var).then(|| "x".to_owned())).unwrap_err();
        assert!(err.to_string().starts_with(var));
    }
}

#[test]
fn finds_the_linux_release_layout() {
    let dir = TempDir::new("rt-linux");
    touch(dir.path(), names(HostOs::Linux).0);
    touch(dir.path(), "libkrunfw.so.5.6.1");
    let pair = RuntimePair::find_for_os(dir.path(), "linux").unwrap();
    assert_eq!(pair.msb, dir.path().join(names(HostOs::Linux).0));
    assert_eq!(pair.libkrunfw, dir.path().join("libkrunfw.so.5.6.1"));
}

#[test]
fn prefers_the_shortest_library_name() {
    let dir = TempDir::new("rt-short");
    touch(dir.path(), names(HostOs::Linux).0);
    touch(dir.path(), "libkrunfw.so.5.6.1");
    touch(dir.path(), names(HostOs::Linux).1);
    let pair = RuntimePair::find_for_os(dir.path(), "linux").unwrap();
    assert_eq!(pair.libkrunfw, dir.path().join(names(HostOs::Linux).1));
}

#[test]
fn finds_the_windows_and_macos_layouts() {
    let dir = TempDir::new("rt-win");
    let (msb, firmware) = names(HostOs::Windows);
    touch(dir.path(), msb);
    touch(dir.path(), firmware);
    let pair = RuntimePair::find_for_os(dir.path(), "windows").unwrap();
    assert_eq!(pair.libkrunfw, dir.path().join(firmware));
    let mac = TempDir::new("rt-mac");
    touch(mac.path(), names(HostOs::MacOs).0);
    touch(mac.path(), &format!("{}dylib", names(HostOs::MacOs).1));
    assert!(RuntimePair::find_for_os(mac.path(), "macos").is_ok());
}

#[test]
fn names_the_missing_half() {
    let dir = TempDir::new("rt-missing");
    let err = RuntimePair::find_for_os(dir.path(), "linux").unwrap_err();
    assert!(matches!(
        err,
        HarnessError::RuntimeIncomplete { missing: "msb", .. }
    ));
    touch(dir.path(), names(HostOs::Linux).0);
    touch(dir.path(), names(HostOs::Windows).1);
    let err = RuntimePair::find_for_os(dir.path(), "linux").unwrap_err();
    assert!(matches!(
        err,
        HarnessError::RuntimeIncomplete {
            missing: "libkrunfw",
            ..
        }
    ));
}

#[test]
fn a_directory_named_like_the_library_does_not_count() {
    let dir = TempDir::new("rt-dir");
    touch(dir.path(), names(HostOs::Linux).0);
    std::fs::create_dir(dir.path().join(names(HostOs::Linux).1)).unwrap();
    assert!(RuntimePair::find_for_os(dir.path(), "linux").is_err());
}

#[test]
fn unsupported_os_is_named() {
    let dir = TempDir::new("rt-os");
    let err = RuntimePair::find_for_os(dir.path(), "freebsd").unwrap_err();
    assert_eq!(err.to_string(), "msb has no runtime for this OS (freebsd)");
}

#[test]
fn the_host_os_is_supported() {
    let dir = TempDir::new("rt-host");
    let err = RuntimePair::find_in(dir.path()).unwrap_err();
    assert!(
        matches!(err, HarnessError::RuntimeIncomplete { .. }),
        "{err}"
    );
}

#[test]
fn config_pins_both_paths_at_version_1() {
    let pair = RuntimePair {
        msb: PathBuf::from("/rt/msb"),
        libkrunfw: PathBuf::from("/rt/libkrunfw.so.5"),
    };
    let value: serde_json::Value = serde_json::from_str(&pair.config_json(None)).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["paths"]["msb"], "/rt/msb");
    assert_eq!(value["paths"]["libkrunfw"], "/rt/libkrunfw.so.5");
    assert_eq!(value.as_object().unwrap().len(), 2);
    let debug: serde_json::Value = serde_json::from_str(&pair.config_json(Some("debug"))).unwrap();
    assert_eq!(debug["log_level"], "debug");
    assert_eq!(debug["paths"], value["paths"]);
}
