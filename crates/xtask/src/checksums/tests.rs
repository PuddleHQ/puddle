// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn temp_file(content: &[u8]) -> tempfile::TempPath {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    std::io::Write::write_all(&mut file, content).unwrap();
    file.into_temp_path()
}

#[test]
fn hashes_files() {
    let empty = temp_file(b"");
    assert_eq!(sha256_file(&empty).unwrap(), EMPTY_SHA);
    let abc = temp_file(b"abc");
    assert_eq!(
        sha256_file(&abc).unwrap(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    std::fs::remove_file(empty).unwrap();
    std::fs::remove_file(abc).unwrap();
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    assert!(sha256_file(Path::new("/nonexistent/xtask/file")).is_err());
}

#[test]
fn parses_sha256sum_output() {
    let upper = EMPTY_SHA.to_ascii_uppercase();
    let text = format!("{EMPTY_SHA}  a.exe\r\n\n{upper} *b.dll\n");
    let sums = Checksums::parse("list", &text).unwrap();
    assert_eq!(sums.sums.get("a.exe").map(String::as_str), Some(EMPTY_SHA));
    assert_eq!(sums.sums.get("b.dll").map(String::as_str), Some(EMPTY_SHA));
}

#[test]
fn rejects_malformed_lists() {
    for bad in [
        "abc  a.exe",
        EMPTY_SHA,
        &format!("{EMPTY_SHA}  "),
        &format!("{}  a.exe", "g".repeat(64)),
        &format!("{EMPTY_SHA}  a.exe\n{EMPTY_SHA}  a.exe"),
    ] {
        assert!(Checksums::parse("list", bad).is_err(), "{bad:?}");
    }
}

#[test]
fn verifies_and_names_the_mismatch() {
    let file = temp_file(b"");
    let sums = Checksums::parse(
        "fork checksums",
        &format!("{EMPTY_SHA}  ok\n{}  bad\n", "0".repeat(64)),
    )
    .unwrap();
    assert_eq!(sums.verify("ok", &file).unwrap(), EMPTY_SHA);
    let err = sums.verify("bad", &file).unwrap_err().to_string();
    assert!(
        err.contains("bad") && err.contains("fork checksums") && err.contains(EMPTY_SHA),
        "{err}"
    );
    let err = sums.verify("missing", &file).unwrap_err().to_string();
    assert_eq!(err, "missing has no entry in fork checksums");
    std::fs::remove_file(file).unwrap();
}
