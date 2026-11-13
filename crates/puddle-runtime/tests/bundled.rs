// SPDX-License-Identifier: GPL-3.0-or-later
//! Opening the bundled runtime: present, readable, exactly the pinned version.
#![expect(
    clippy::unwrap_used,
    reason = "fixture helpers run outside #[test] but only in tests"
)]

mod support;

use puddle_runtime::{
    BUILT_FOR, BundledRuntime, DevOverride, DigestStatus, RuntimeDigests, RuntimeError,
    RuntimeLayout, RuntimeVersion, VersionStatus, sha256_file,
};
use support::{TempDir, fake_msb};

fn layout_with(dir: &TempDir, msb: Option<&[u8]>) -> RuntimeLayout {
    let layout = RuntimeLayout::new(dir.path().join("runtime"), dir.path().join("home")).unwrap();
    std::fs::create_dir_all(layout.runtime_dir()).unwrap();
    if let Some(bytes) = msb {
        std::fs::write(layout.msb_path(), bytes).unwrap();
    }
    std::fs::write(layout.libkrunfw_path(), b"firmware").unwrap();
    layout
}

/// The hashes of the files as `layout_with` wrote them: an untouched install.
fn digests_of(layout: &RuntimeLayout) -> RuntimeDigests {
    RuntimeDigests::pinned(
        sha256_file(&layout.msb_path()).unwrap(),
        sha256_file(&layout.libkrunfw_path()).unwrap(),
    )
}

#[test]
fn the_pinned_version_opens() {
    let dir = TempDir::new("open");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    let rt = BundledRuntime::open_with(
        layout.clone(),
        &RuntimeVersion::built_for(),
        &digests_of(&layout),
        DevOverride::none(),
    )
    .unwrap();
    assert_eq!(rt.status(), &VersionStatus::Exact);
    assert_eq!(rt.digest_status(), &DigestStatus::Verified);
    assert_eq!(rt.layout(), &layout);
    assert_eq!(rt.command().get_program(), layout.msb_path().as_os_str());
}

#[test]
fn a_mismatch_is_refused_naming_both_versions() {
    let dir = TempDir::new("mismatch");
    let expected: RuntimeVersion = "0.7.7-puddle.2".parse().unwrap();
    for found in ["0.7.7", "0.7.7-puddle.1", "0.7.8-puddle.2"] {
        let layout = layout_with(&dir, Some(&fake_msb(Some(found))));
        let err = BundledRuntime::open(layout.clone(), &expected, DevOverride::none()).unwrap_err();
        assert_eq!(
            err,
            RuntimeError::Mismatch {
                path: layout.msb_path(),
                expected: "0.7.7-puddle.2".into(),
                found: found.into(),
            }
        );
        let msg = err.to_string();
        assert!(
            msg.contains(found) && msg.contains("0.7.7-puddle.2"),
            "{msg}"
        );
    }
}

#[test]
fn a_binary_without_version_is_refused() {
    let dir = TempDir::new("nover");
    let layout = layout_with(&dir, Some(&fake_msb(None)));
    let err = BundledRuntime::open(layout, &RuntimeVersion::built_for(), DevOverride::none())
        .unwrap_err();
    assert!(matches!(err, RuntimeError::NoVersion { ref expected, .. } if expected == BUILT_FOR));
    assert!(err.to_string().contains("no embedded version"));
}

#[test]
fn a_missing_or_unreadable_binary_is_refused() {
    let dir = TempDir::new("missing");
    let layout = layout_with(&dir, None);
    let err = BundledRuntime::open(
        layout.clone(),
        &RuntimeVersion::built_for(),
        DevOverride::none(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        RuntimeError::Missing {
            path: layout.msb_path()
        }
    );
    assert!(err.to_string().contains("reinstall"));

    let layout = layout_with(&dir, Some(b"MZ not really a binary"));
    let err = BundledRuntime::open(layout, &RuntimeVersion::built_for(), DevOverride::none())
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Unreadable { .. }), "{err:?}");
}

#[test]
fn a_directory_named_msb_is_missing_not_unreadable() {
    let dir = TempDir::new("dir");
    let layout = layout_with(&dir, None);
    std::fs::create_dir_all(layout.msb_path()).unwrap();
    let err = BundledRuntime::open(layout, &RuntimeVersion::built_for(), DevOverride::none())
        .unwrap_err();
    assert!(matches!(err, RuntimeError::Missing { .. }));
}

#[test]
fn the_dev_override_accepts_a_mismatch_only_when_compiled_in() {
    let dir = TempDir::new("override");
    let layout = layout_with(&dir, Some(&fake_msb(Some("0.9.0"))));
    let got = BundledRuntime::open(
        layout,
        &RuntimeVersion::built_for(),
        DevOverride::requested(),
    );
    if puddle_runtime::DEV_OVERRIDE_COMPILED {
        assert_eq!(
            got.unwrap().status(),
            &VersionStatus::Overridden {
                found: Some("0.9.0".into())
            }
        );
    } else {
        assert!(matches!(got, Err(RuntimeError::Mismatch { .. })));
    }
}

#[cfg(windows)]
#[test]
fn a_missing_firmware_library_is_refused_on_windows() {
    let dir = TempDir::new("nofw");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    std::fs::remove_file(layout.libkrunfw_path()).unwrap();
    let err = BundledRuntime::open(
        layout.clone(),
        &RuntimeVersion::built_for(),
        DevOverride::none(),
    )
    .unwrap_err();
    assert_eq!(
        err,
        RuntimeError::Missing {
            path: layout.libkrunfw_path()
        }
    );
}

#[test]
fn a_swapped_file_refuses_the_start_naming_it_and_both_hashes() {
    let dir = TempDir::new("swapped");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    let digests = digests_of(&layout);
    let version = RuntimeVersion::built_for();
    for swapped in [layout.msb_path(), layout.libkrunfw_path()] {
        let before = std::fs::read(&swapped).unwrap();
        let expected = sha256_file(&swapped).unwrap();
        let mut bytes = before.clone();
        bytes.push(0);
        std::fs::write(&swapped, &bytes).unwrap();
        let err =
            BundledRuntime::open_with(layout.clone(), &version, &digests, DevOverride::none())
                .unwrap_err();
        let found = sha256_file(&swapped).unwrap();
        assert_eq!(
            err,
            RuntimeError::Digest {
                path: swapped.clone(),
                expected: expected.clone(),
                found: found.clone(),
            }
        );
        let message = err.to_string();
        for part in [&swapped.display().to_string(), &expected, &found] {
            assert!(message.contains(part), "{message}");
        }
        std::fs::write(&swapped, &before).unwrap();
        BundledRuntime::open_with(layout.clone(), &version, &digests, DevOverride::none()).unwrap();
    }
}

#[test]
fn the_version_is_refused_before_the_bytes_are_hashed() {
    let dir = TempDir::new("order");
    let layout = layout_with(&dir, Some(&fake_msb(Some("0.7.7"))));
    let err = BundledRuntime::open_with(
        layout.clone(),
        &RuntimeVersion::built_for(),
        &digests_of(&layout),
        DevOverride::none(),
    )
    .unwrap_err();
    assert!(matches!(err, RuntimeError::Mismatch { .. }), "{err:?}");
}

#[test]
fn the_dev_override_bypasses_the_hash_check_when_compiled_in() {
    let dir = TempDir::new("override-hash");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    let digests = digests_of(&layout);
    std::fs::write(layout.msb_path(), fake_msb(Some(BUILT_FOR)).repeat(2)).unwrap();
    let got = BundledRuntime::open_with(
        layout.clone(),
        &RuntimeVersion::built_for(),
        &digests,
        DevOverride::requested(),
    );
    if puddle_runtime::DEV_OVERRIDE_COMPILED {
        let rt = got.unwrap();
        assert_eq!(rt.status(), &VersionStatus::Exact);
        assert_eq!(
            rt.digest_status(),
            &DigestStatus::Overridden {
                files: vec![layout.msb_path()]
            }
        );
    } else {
        assert!(matches!(got, Err(RuntimeError::Digest { .. })));
    }
}

#[test]
fn a_runtime_with_no_expected_hashes_opens_unchecked() {
    let dir = TempDir::new("unpinned");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    let rt = BundledRuntime::open_with(
        layout,
        &RuntimeVersion::built_for(),
        &RuntimeDigests::default(),
        DevOverride::none(),
    )
    .unwrap();
    assert_eq!(rt.digest_status(), &DigestStatus::Unpinned);
}

#[test]
fn open_checks_against_the_built_in_hashes() {
    // The fake files are not what any release ships, so on a target the pin list covers the
    // built-in check must refuse them, and elsewhere there is nothing to check.
    let dir = TempDir::new("built-in");
    let layout = layout_with(&dir, Some(&fake_msb(Some(BUILT_FOR))));
    let got = BundledRuntime::open(layout, &RuntimeVersion::built_for(), DevOverride::none());
    if cfg!(any(windows, target_os = "linux")) && cfg!(target_arch = "x86_64") {
        assert!(matches!(got, Err(RuntimeError::Digest { .. })), "{got:?}");
    } else {
        assert_eq!(got.unwrap().digest_status(), &DigestStatus::Unpinned);
    }
}
