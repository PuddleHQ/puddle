// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn built_for_is_a_valid_fork_version() {
    let v = RuntimeVersion::built_for();
    assert_eq!(v.to_string(), BUILT_FOR);
    assert_eq!(v.fork_tag(), format!("v{BUILT_FOR}"));
    assert!(v.puddle_revision() >= 1);
}

#[test]
fn parses_and_round_trips() {
    let v: RuntimeVersion = "0.7.8-puddle.2".parse().unwrap();
    assert_eq!(v.release(), "0.7.8");
    assert_eq!(v.puddle_revision(), 2);
    assert_eq!(v.to_string(), "0.7.8-puddle.2");
    assert_eq!(v.fork_tag(), "v0.7.8-puddle.2");
    assert_eq!(v.upstream_tag(), "v0.7.8");
    let big: RuntimeVersion = "10.20.30-puddle.1".parse().unwrap();
    assert_eq!(big.release(), "10.20.30");
}

#[test]
fn rejects_everything_but_release_dash_puddle_n() {
    for bad in [
        "",
        "0.7.7",
        "v0.7.7-puddle.1",
        "0.7-puddle.1",
        "0.7.7.1-puddle.1",
        "0.7.7-puddle.0",
        "0.7.7-puddle.01",
        "00.7.7-puddle.1",
        "0.07.7-puddle.1",
        "0.7.7-puddle.",
        "0.7.7-puddle.1+build",
        "0.7.7-puddle.1 ",
        " 0.7.7-puddle.1",
        "0.7.7-Puddle.1",
        "0.7.7-rc.1",
        "0.7.x-puddle.1",
        "0.7.7-puddle.-1",
        "0.7.7-puddle.1.2",
        "0.7.7-puddle.99999999999",
        "+1.7.7-puddle.1",
    ] {
        assert!(bad.parse::<RuntimeVersion>().is_err(), "accepted {bad:?}");
    }
}

#[test]
fn parse_error_echoes_a_printable_value() {
    let err = "bad\u{1b}[31m".parse::<RuntimeVersion>().unwrap_err();
    let msg = err.to_string();
    assert!(!msg.contains('\u{1b}'), "{msg}");
    assert!(msg.contains("\\u{1b}"), "{msg}");
}

#[test]
fn printable_cuts_long_values() {
    let long = "9".repeat(500);
    let p = printable(&long);
    assert_eq!(p.chars().count(), 64);
    assert!(p.ends_with("..."));
    assert_eq!(printable("0.7.7"), "0.7.7");
}

#[test]
fn exact_version_passes() {
    let v: RuntimeVersion = "0.7.7-puddle.2".parse().unwrap();
    assert_eq!(
        check_version(&v, Some("0.7.7-puddle.2"), DevOverride::none()),
        Ok(VersionStatus::Exact)
    );
    // The override never changes an exact match.
    assert_eq!(
        check_version(&v, Some("0.7.7-puddle.2"), DevOverride::requested()),
        Ok(VersionStatus::Exact)
    );
}

#[test]
fn any_other_version_is_refused() {
    let v: RuntimeVersion = "0.7.7-puddle.2".parse().unwrap();
    for other in [
        "0.7.7",
        "0.7.7-puddle.1",
        "0.7.7-puddle.3",
        "0.7.6-puddle.2",
        "0.8.0-puddle.2",
        "0.7.7-puddle.2+x",
        "0.7.7-puddle.2\n",
    ] {
        assert_eq!(
            check_version(&v, Some(other), DevOverride::none()),
            Err(VersionRefusal::Mismatch(printable(other))),
            "{other:?}"
        );
    }
    assert_eq!(
        check_version(&v, None, DevOverride::none()),
        Err(VersionRefusal::NoVersion)
    );
}

#[test]
fn dev_override_only_works_when_compiled_in() {
    let v: RuntimeVersion = "0.7.7-puddle.2".parse().unwrap();
    let got = check_version(&v, Some("0.7.8"), DevOverride::requested());
    if DEV_OVERRIDE_COMPILED {
        assert_eq!(
            got,
            Ok(VersionStatus::Overridden {
                found: Some("0.7.8".into())
            })
        );
        assert_eq!(
            check_version(&v, None, DevOverride::requested()),
            Ok(VersionStatus::Overridden { found: None })
        );
    } else {
        assert_eq!(got, Err(VersionRefusal::Mismatch("0.7.8".into())));
    }
    assert!(!DevOverride::none().active());
    assert_eq!(DevOverride::requested().active(), DEV_OVERRIDE_COMPILED);
}
