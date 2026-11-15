// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn clip_keeps_short_values_and_cuts_long_ones() {
    assert_eq!(clip("abc"), "abc");
    assert_eq!(clip(&"x".repeat(100)).len(), 64);
}

#[test]
fn messages_name_what_to_fix() {
    let missing = HarnessError::MissingVar {
        var: "PUDDLE_VM_RUNTIME_DIR",
        hint: "a directory with msb and libkrunfw",
    };
    assert_eq!(
        missing.to_string(),
        "PUDDLE_VM_RUNTIME_DIR is not set: a directory with msb and libkrunfw"
    );
    let ambient = HarnessError::AmbientMsbVar { var: "MSB_HOME" };
    assert!(ambient.to_string().starts_with("MSB_HOME is set"));
    let timeout = HarnessError::Timeout {
        what: "create",
        limit: Duration::from_secs(3),
    };
    assert_eq!(timeout.to_string(), "create took longer than 3s");
    let level = HarnessError::InvalidLogLevel {
        value: "loud".into(),
    };
    assert!(level.to_string().contains("\"loud\": use error, warn"));
    let release = HarnessError::VsCodeRelease { dir: "/v".into() };
    assert!(release.to_string().starts_with("/v has no app/Code.exe"));
    let io = HarnessError::io("create", "/x", std::io::Error::other("boom"));
    assert_eq!(io.to_string(), "create /x: boom");
}
