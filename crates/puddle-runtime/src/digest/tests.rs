// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn layout(dir: &Path) -> RuntimeLayout {
    let layout = RuntimeLayout::new(dir.join("runtime"), dir.join("home")).unwrap();
    std::fs::create_dir_all(layout.runtime_dir()).unwrap();
    layout
}

fn hash_of(bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    std::fs::write(&path, bytes).unwrap();
    sha256_file(&path).unwrap()
}

#[test]
fn hashes_match_the_published_test_vectors() {
    assert_eq!(hash_of(b"abc"), ABC);
    assert_eq!(hash_of(b""), EMPTY);
}

#[test]
fn a_file_larger_than_the_read_buffer_is_hashed_whole() {
    // 2.5 buffers of a repeating pattern: a reader that stopped after one read would differ.
    let big: Vec<u8> = (0..(5 << 19))
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let mut expected = String::new();
    for byte in Sha256::digest(&big) {
        write!(expected, "{byte:02x}").unwrap();
    }
    assert_eq!(hash_of(&big), expected);
}

#[test]
fn the_built_in_hashes_come_from_the_pin_list() {
    let built_in = RuntimeDigests::built_in();
    let covered = cfg!(any(windows, target_os = "linux")) && cfg!(target_arch = "x86_64");
    assert_eq!(built_in.msb.is_some(), covered);
    assert_eq!(built_in.firmware.is_some(), covered);
    for hash in built_in.msb.iter().chain(&built_in.firmware) {
        assert_eq!(hash.len(), 64, "{hash}");
    }
}

#[test]
fn nothing_expected_checks_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    assert_eq!(
        verify(&layout, &RuntimeDigests::default(), DevOverride::none()),
        Ok(DigestStatus::Unpinned)
    );
}

#[test]
fn files_with_the_expected_hashes_pass() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abc").unwrap();
    std::fs::write(layout.libkrunfw_path(), b"").unwrap();
    // Upper case in the list is the same hash.
    let digests = RuntimeDigests::pinned(ABC.to_ascii_uppercase(), EMPTY);
    assert_eq!(
        verify(&layout, &digests, DevOverride::none()),
        Ok(DigestStatus::Verified)
    );
}

#[test]
fn a_swapped_msb_is_refused_naming_the_file_and_both_hashes() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abz").unwrap();
    std::fs::write(layout.libkrunfw_path(), b"").unwrap();
    let err = verify(
        &layout,
        &RuntimeDigests::pinned(ABC, EMPTY),
        DevOverride::none(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        RuntimeError::Digest {
            path: layout.msb_path(),
            expected: ABC.into(),
            found: hash_of(b"abz"),
        }
    );
    let message = err.to_string();
    for part in [
        layout.msb_path().display().to_string(),
        ABC.into(),
        hash_of(b"abz"),
        "repair or reinstall Puddle".into(),
    ] {
        assert!(message.contains(&part), "{message}");
    }
}

#[test]
fn a_swapped_firmware_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abc").unwrap();
    std::fs::write(layout.libkrunfw_path(), b"x").unwrap();
    let err = verify(
        &layout,
        &RuntimeDigests::pinned(ABC, EMPTY),
        DevOverride::none(),
    )
    .unwrap_err();
    assert!(
        matches!(&err, RuntimeError::Digest { path, .. } if *path == layout.libkrunfw_path()),
        "{err:?}"
    );
}

#[test]
fn a_firmware_name_that_is_absent_is_left_to_msb() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abc").unwrap();
    assert_eq!(
        verify(
            &layout,
            &RuntimeDigests::pinned(ABC, EMPTY),
            DevOverride::none()
        ),
        Ok(DigestStatus::Verified)
    );
}

#[test]
fn an_unreadable_file_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    let err = verify(
        &layout,
        &RuntimeDigests::pinned(ABC, EMPTY),
        DevOverride::none(),
    )
    .unwrap_err();
    assert!(
        matches!(&err, RuntimeError::Unreadable { path, .. } if *path == layout.msb_path()),
        "{err:?}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn linux_checks_the_versioned_copy_msb_loads_too() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abc").unwrap();
    std::fs::write(layout.libkrunfw_path(), b"").unwrap();
    let versioned = layout.runtime_dir().join("libkrunfw.so.5.6.1");
    std::fs::write(&versioned, b"tampered").unwrap();
    let digests = RuntimeDigests {
        firmware_version: Some("5.6.1".into()),
        ..RuntimeDigests::pinned(ABC, EMPTY)
    };
    let err = verify(&layout, &digests, DevOverride::none()).unwrap_err();
    assert!(
        matches!(&err, RuntimeError::Digest { path, .. } if *path == versioned),
        "{err:?}"
    );
    std::fs::write(&versioned, b"").unwrap();
    assert_eq!(
        verify(&layout, &digests, DevOverride::none()),
        Ok(DigestStatus::Verified)
    );
}

#[test]
fn the_dev_override_accepts_other_bytes_and_names_them() {
    let dir = tempfile::tempdir().unwrap();
    let layout = layout(dir.path());
    std::fs::write(layout.msb_path(), b"abz").unwrap();
    std::fs::write(layout.libkrunfw_path(), b"").unwrap();
    let got = verify(
        &layout,
        &RuntimeDigests::pinned(ABC, EMPTY),
        DevOverride::requested(),
    );
    if crate::DEV_OVERRIDE_COMPILED {
        assert_eq!(
            got,
            Ok(DigestStatus::Overridden {
                files: vec![layout.msb_path()]
            })
        );
    } else {
        assert!(matches!(got, Err(RuntimeError::Digest { .. })));
    }
}
