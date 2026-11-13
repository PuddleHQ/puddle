// SPDX-License-Identifier: GPL-3.0-or-later
//! [`SystemProbe`] on this machine against runtime folders built per test: what it reports for a
//! missing, wrong-version, unexecutable and right-version msb, and that `diagnose` only starts msb
//! when the runtime check passed. The real msb is booted by `vm_doctor` (puddle-vm-tests).
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::path::Path;
use std::time::Duration;

use object::write::{Object, StandardSection};
use object::{Architecture, BinaryFormat, Endianness, SectionKind};
use puddle_doctor::{
    CheckId, DataEncryption, DiskFacts, Finding, Options, Probe, ProcessOutcome, RuntimeState,
    Status, SystemProbe, diagnose,
};
use puddle_runtime::{
    DevOverride, LIBKRUNFW_FILE_NAME, MSB_FILE_NAME, RUNTIME_DIR_NAME, RuntimeDigests,
    RuntimeError, RuntimeVersion, sha256_file,
};

const EXPECTED: &str = "0.7.7-puddle.2";

fn expected() -> RuntimeVersion {
    EXPECTED.parse().unwrap()
}

/// An object file with msb's `.msbver` section: enough for the version reader, not runnable.
fn fake_msb(version: &str) -> Vec<u8> {
    let format = if cfg!(windows) {
        BinaryFormat::Coff
    } else {
        BinaryFormat::Elf
    };
    let mut obj = Object::new(format, Architecture::X86_64, Endianness::Little);
    let text = obj.section_id(StandardSection::Text);
    obj.append_section_data(text, &[0xc3], 1);
    let id = obj.add_section(Vec::new(), b".msbver".to_vec(), SectionKind::ReadOnlyData);
    obj.append_section_data(id, version.as_bytes(), 1);
    obj.write().unwrap()
}

/// A runtime folder holding a fake msb of `version` (and the firmware file Windows requires).
fn runtime_dir(root: &Path, version: &str) -> std::path::PathBuf {
    let dir = root.join(RUNTIME_DIR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    let msb = dir.join(MSB_FILE_NAME);
    #[cfg(unix)]
    puddle_test_kit::write_executable_bytes(&msb, &fake_msb(version));
    #[cfg(not(unix))]
    std::fs::write(&msb, fake_msb(version)).unwrap();
    std::fs::write(dir.join(LIBKRUNFW_FILE_NAME), b"firmware").unwrap();
    dir
}

/// The hashes of the files `runtime_dir` wrote: an untouched install.
fn digests_of(dir: &Path) -> RuntimeDigests {
    RuntimeDigests::pinned(
        sha256_file(&dir.join(MSB_FILE_NAME)).unwrap(),
        sha256_file(&dir.join(LIBKRUNFW_FILE_NAME)).unwrap(),
    )
}

fn no_boot() -> Options {
    Options {
        boot: false,
        launch_limit: Duration::from_secs(10),
        budget: Duration::from_secs(28),
        ..Options::default()
    }
}

#[test]
fn installed_looks_for_the_runtime_next_to_the_program() {
    let root = tempfile::tempdir().unwrap();
    let probe = SystemProbe::installed(&root.path().join("puddle.exe")).unwrap();
    let facts = probe.runtime();
    assert_eq!(facts.dir, root.path().join(RUNTIME_DIR_NAME));
    assert_eq!(facts.msb, facts.dir.join(MSB_FILE_NAME));
    assert_eq!(facts.expected, RuntimeVersion::built_for().to_string());
    assert!(matches!(
        SystemProbe::installed(Path::new("")),
        Err(RuntimeError::NoExeDir { .. })
    ));
}

#[test]
fn a_missing_runtime_is_reported_and_msb_never_started() {
    let root = tempfile::tempdir().unwrap();
    let probe = SystemProbe::new(root.path().join("runtime"), expected(), DevOverride::none());
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(
        runtime.finding,
        Some(Finding::RuntimeMissing),
        "{runtime:?}"
    );
    assert_eq!(
        report.check(CheckId::Launch).unwrap().status,
        Status::Skipped
    );
    assert!(!report.ok);
}

#[test]
fn a_relative_runtime_folder_is_unusable() {
    let probe = SystemProbe::new("runtime".into(), expected(), DevOverride::none());
    assert!(matches!(
        probe.runtime().state,
        RuntimeState::Unusable(RuntimeError::RelativePath { .. })
    ));
}

#[test]
fn another_version_is_a_mismatch_naming_both() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), "0.7.7");
    let probe = SystemProbe::new(dir, expected(), DevOverride::none());
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(runtime.finding, Some(Finding::RuntimeVersionMismatch));
    assert!(runtime.summary.contains("0.7.7"), "{}", runtime.summary);
    assert!(runtime.summary.contains(EXPECTED), "{}", runtime.summary);
}

#[test]
fn the_right_version_is_ready_and_then_started() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), EXPECTED);
    let probe = SystemProbe::new(dir.clone(), expected(), DevOverride::none())
        .with_digests(digests_of(&dir));
    // Before the runtime check, nothing may run.
    assert!(matches!(
        probe.launch(Duration::from_secs(5)),
        ProcessOutcome::SpawnFailed { os_error: None, .. }
    ));
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(runtime.status, Status::Ok, "{runtime:?}");
    assert!(runtime.summary.contains(EXPECTED));
    // The fake isn't a program, so starting it fails; what matters is that it was attempted.
    let launch = report.check(CheckId::Launch).unwrap();
    assert_eq!(launch.status, Status::Fail, "{launch:?}");
    assert_eq!(
        report.check(CheckId::TestBoot).unwrap().summary,
        "not checked: --no-boot"
    );
}

#[test]
fn the_dev_override_counts_only_when_compiled_in() {
    // puddle-doctor never enables puddle-runtime's `dev-override` feature; `--all-features` does.
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), "0.7.8");
    let probe = SystemProbe::new(dir, expected(), DevOverride::requested());
    let state = probe.runtime().state;
    if puddle_runtime::DEV_OVERRIDE_COMPILED {
        assert_eq!(
            state,
            RuntimeState::Ready {
                version: "0.7.8".into(),
                overridden: true
            }
        );
    } else {
        assert!(
            matches!(state, RuntimeState::Unusable(RuntimeError::Mismatch { .. })),
            "{state:?}"
        );
    }
}

#[test]
fn a_swapped_firmware_is_a_digest_finding_naming_the_file_and_both_hashes() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), EXPECTED);
    let digests = digests_of(&dir);
    let firmware = dir.join(LIBKRUNFW_FILE_NAME);
    let expected_hash = sha256_file(&firmware).unwrap();
    std::fs::write(&firmware, b"swapped").unwrap();
    let found_hash = sha256_file(&firmware).unwrap();
    let probe = SystemProbe::new(dir, expected(), DevOverride::none()).with_digests(digests);
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(
        runtime.finding,
        Some(Finding::RuntimeDigestMismatch),
        "{runtime:?}"
    );
    for part in [firmware.display().to_string(), expected_hash, found_hash] {
        assert!(runtime.summary.contains(&part), "{}", runtime.summary);
    }
    assert_eq!(
        report.check(CheckId::Launch).unwrap().status,
        Status::Skipped
    );
    assert!(!report.ok);
}

#[test]
fn the_dev_override_accepts_swapped_files_and_says_so() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), EXPECTED);
    let digests = digests_of(&dir);
    std::fs::write(dir.join(LIBKRUNFW_FILE_NAME), b"swapped").unwrap();
    let probe = SystemProbe::new(dir, expected(), DevOverride::requested()).with_digests(digests);
    let state = probe.runtime().state;
    if puddle_runtime::DEV_OVERRIDE_COMPILED {
        assert_eq!(
            state,
            RuntimeState::Ready {
                version: EXPECTED.into(),
                overridden: true
            }
        );
    } else {
        assert!(
            matches!(state, RuntimeState::Unusable(RuntimeError::Digest { .. })),
            "{state:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn an_unexecutable_msb_is_a_permissions_finding() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_dir(root.path(), EXPECTED);
    let msb = dir.join(MSB_FILE_NAME);
    std::fs::set_permissions(&msb, std::fs::Permissions::from_mode(0o644)).unwrap();
    let probe = SystemProbe::new(dir, expected(), DevOverride::none());
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(
        runtime.finding,
        Some(Finding::RuntimePermissions),
        "{runtime:?}"
    );
    assert!(
        runtime
            .fix
            .as_deref()
            .unwrap()
            .contains(&*msb.to_string_lossy())
    );
    assert_eq!(
        report.check(CheckId::Launch).unwrap().status,
        Status::Skipped
    );
}

#[test]
fn the_machine_probes_answer() {
    let probe = SystemProbe::new(
        std::path::absolute("runtime").unwrap(),
        expected(),
        DevOverride::none(),
    );
    assert_eq!(probe.arch(), std::env::consts::ARCH);
    let _hv = probe.hypervisor();
    assert_eq!(probe.code_integrity().is_some(), cfg!(windows));
    assert_eq!(probe.global_secure_access().is_some(), cfg!(windows));
    let _job = probe.job();
}

#[test]
fn boot_uses_the_probes_runtime_folder() {
    // A runtime folder without msb: the test boot starts nothing and says why.
    let root = tempfile::tempdir().unwrap();
    let probe = SystemProbe::new(root.path().to_path_buf(), expected(), DevOverride::none());
    let facts = probe.boot(Duration::from_secs(5));
    if std::env::consts::ARCH == "x86_64" {
        let puddle_doctor::BootFacts::Ran {
            outcome: ProcessOutcome::SpawnFailed { os_error, .. },
            ..
        } = facts
        else {
            panic!("unexpected {facts:?}");
        };
        assert_eq!(os_error, Some(2));
    }
}

#[test]
fn the_real_probe_measures_the_drive_of_the_data_folder_even_before_the_folder_exists() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("not-yet").join("puddle");
    let probe = SystemProbe::new(root.path().join("runtime"), expected(), DevOverride::none())
        .with_data_dir(&data);
    let DiskFacts::Read { dir, free_bytes } = probe.disk_space() else {
        panic!("a temp folder's drive can be measured");
    };
    assert_eq!(dir, data);
    assert!(free_bytes > 0);
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let disk = report.check(CheckId::DiskSpace).unwrap();
    assert!(
        disk.summary
            .contains("free on the drive that holds Puddle's data"),
        "{disk:?}"
    );
}

#[test]
fn a_probe_without_a_data_folder_has_nothing_to_measure() {
    let probe = SystemProbe::new("runtime".into(), expected(), DevOverride::none());
    assert_eq!(probe.disk_space(), DiskFacts::NoFolder);
}

#[test]
fn a_data_folder_the_os_cannot_measure_is_unreadable_not_a_failure() {
    let probe =
        SystemProbe::new("runtime".into(), expected(), DevOverride::none()).with_data_dir("");
    let DiskFacts::Unreadable { detail, .. } = probe.disk_space() else {
        panic!("an empty path has no drive");
    };
    assert!(detail.contains("do not exist"), "{detail}");
}

#[test]
fn the_real_probe_reports_encryption_for_the_data_folder_without_failing() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("not-yet").join("puddle");
    let probe = SystemProbe::new(root.path().join("runtime"), expected(), DevOverride::none())
        .with_data_dir(&data);
    let DataEncryption::Volume { dir, .. } = probe.disk_encryption().data else {
        panic!("a folder was named");
    };
    assert_eq!(dir, data);
    let report = diagnose(&probe, &no_boot(), "0.1.0");
    let check = report.check(CheckId::DiskEncryption).unwrap();
    assert_ne!(check.status, Status::Fail, "{check:?}");
    assert_eq!(
        SystemProbe::new("runtime".into(), expected(), DevOverride::none())
            .disk_encryption()
            .data,
        DataEncryption::NoFolder
    );
}
