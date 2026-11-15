// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::PLACEHOLDER_SEED;

const NOW: u64 = 1_800_000_000;
const DAY: u64 = 86_400;
const OTHER: [u8; 32] = [0x51; 32];

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[test]
fn the_first_refresh_makes_sequence_one_and_each_later_one_adds_one() {
    let d = dir();
    let file = d.path().join("freshness.json");
    let line = refresh_file(&file, &PLACEHOLDER_SEED, NOW, DEFAULT_VALIDITY_SECS).unwrap();
    assert!(line.contains("sequence 1"), "{line}");
    let line = refresh_file(&file, &PLACEHOLDER_SEED, NOW + DAY, DEFAULT_VALIDITY_SECS).unwrap();
    assert!(line.contains("sequence 2"), "{line}");
    assert!(!d.path().join("freshness.json.tmp").exists());
    let key = public_key_text(&PLACEHOLDER_SEED);
    let ok = verify_file(&file, &key, NOW + DAY).unwrap();
    assert!(
        ok.contains("sequence 2") && ok.contains(&format!("{} s left", 7 * DAY)),
        "{ok}"
    );
}

#[test]
fn a_refresh_over_a_file_of_another_key_changes_nothing() {
    let d = dir();
    let file = d.path().join("freshness.json");
    refresh_file(&file, &OTHER, NOW, DEFAULT_VALIDITY_SECS).unwrap();
    let before = std::fs::read(&file).unwrap();
    let err = refresh_file(&file, &PLACEHOLDER_SEED, NOW + DAY, DEFAULT_VALIDITY_SECS).unwrap_err();
    assert!(matches!(err, CliError::Freshness(_)), "{err}");
    assert_eq!(std::fs::read(&file).unwrap(), before);
}

#[test]
fn a_refresh_cannot_read_a_folder_or_write_into_a_missing_one() {
    let d = dir();
    let err = refresh_file(d.path(), &PLACEHOLDER_SEED, NOW, DAY).unwrap_err();
    assert!(err.to_string().starts_with("reading "), "{err}");
    let err = refresh_file(
        &d.path().join("no/such/freshness.json"),
        &PLACEHOLDER_SEED,
        NOW,
        DAY,
    )
    .unwrap_err();
    assert!(err.to_string().starts_with("writing "), "{err}");
}

#[test]
fn verify_refuses_an_expired_file_another_key_and_a_bad_key() {
    let d = dir();
    let file = d.path().join("freshness.json");
    refresh_file(&file, &PLACEHOLDER_SEED, NOW, DEFAULT_VALIDITY_SECS).unwrap();
    let key = public_key_text(&PLACEHOLDER_SEED);
    let err = verify_file(&file, &key, NOW + 8 * DAY).unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
    let err = verify_file(&file, &public_key_text(&OTHER), NOW).unwrap_err();
    assert!(err.to_string().contains("not from a trusted key"), "{err}");
    for bad in ["", "!!!", "AAAA"] {
        let err = verify_file(&file, bad, NOW).unwrap_err();
        assert!(matches!(err, CliError::Key { .. }), "{bad}: {err}");
    }
    // Some 32-byte values are not curve points.
    let off_curve = (0..=u8::MAX)
        .map(|b| STANDARD.encode([b; 32]))
        .find(|k| matches!(verify_file(&file, k, NOW), Err(CliError::Key { .. })));
    assert!(off_curve.is_some());
    let err = verify_file(&d.path().join("missing.json"), &key, NOW).unwrap_err();
    assert!(err.to_string().starts_with("reading "), "{err}");
}

#[test]
fn a_seed_is_32_bytes_of_base64() {
    let text = STANDARD.encode(PLACEHOLDER_SEED);
    assert_eq!(parse_seed(&format!(" {text}\n")).unwrap(), PLACEHOLDER_SEED);
    for bad in ["", "not base64!", &STANDARD.encode([1_u8; 31])] {
        assert!(
            matches!(parse_seed(bad), Err(CliError::Key { .. })),
            "{bad}"
        );
    }
}

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn the_command_refreshes_verifies_and_prints_the_key() {
    let dir = dir();
    let file = dir.path().join("f.json");
    let file = file.to_str().unwrap();
    let key = run(args(&["public-key", "--placeholder-key"])).unwrap();
    let line = run(args(&[
        "refresh",
        "--placeholder-key",
        "--file",
        file,
        "--now",
        "1800000000",
    ]))
    .unwrap();
    assert!(line.contains("sequence 1"), "{line}");
    let line = run(args(&[
        "refresh",
        "--placeholder-key",
        "--file",
        file,
        "--now",
        "1800086400",
        "--days",
        "3",
    ]))
    .unwrap();
    assert!(
        line.contains("sequence 2") && line.contains("259200 s"),
        "{line}"
    );
    let line = run(args(&[
        "verify",
        "--key",
        &key,
        "--file",
        file,
        "--now",
        "1800086400",
    ]))
    .unwrap();
    assert!(line.contains("freshness ok"), "{line}");
    // Without --now the system clock decides: a file issued in 2001 has long expired.
    run(args(&[
        "refresh",
        "--placeholder-key",
        "--file",
        file,
        "--now",
        "1000000000",
        "--days",
        "1",
    ]))
    .unwrap();
    let err = run(args(&["verify", "--key", &key, "--file", file])).unwrap_err();
    assert!(err.to_string().contains("expired"), "{err}");
}

#[test]
fn usage_errors() {
    for (input, needle) in [
        (vec![], "no command"),
        (vec!["sign"], "unknown command sign"),
        (vec!["verify"], "verify needs --key"),
        (vec!["refresh"], "--seed-env"),
        (
            vec!["refresh", "--seed-env", "PUDDLE_NO_SUCH_SEED"],
            "is not set",
        ),
        (vec!["verify", "--key", "x", "--now", "soon"], "--now"),
        (
            vec!["refresh", "--placeholder-key", "--days", "x"],
            "--days",
        ),
        (vec!["verify", "--bogus"], "unknown argument --bogus"),
        (vec!["verify", "--key"], "--key needs a value"),
    ] {
        let err = run(args(&input)).unwrap_err();
        assert!(matches!(err, CliError::Usage(_)), "{input:?}: {err}");
        assert!(
            err.to_string().contains(needle) && err.to_string().contains("usage: puddle-freshness"),
            "{input:?}: {err}"
        );
    }
    // PATH is always set and is not a base64 seed.
    let err = run(args(&["public-key", "--seed-env", "PATH"])).unwrap_err();
    assert!(matches!(err, CliError::Key { .. }), "{err}");
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_utf8_is_a_usage_error() {
    use std::os::unix::ffi::OsStringExt;
    let err = run([OsString::from_vec(vec![0xff, 0xfe])]).unwrap_err();
    assert!(err.to_string().contains("not UTF-8"), "{err}");
}
