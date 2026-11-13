// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle doctor` on a real host with the real runtime (tiers K and W): all green with
//! the runtime CI installed, including a test boot, in under 30 s; and a missing runtime, another
//! version and a runtime file this user may not run (a deny ACE on Windows, no execute bit on
//! Linux) each give their own finding on the real probes. Runs only under `--profile vm`.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stderr,
    reason = "test helpers outside #[test] fns fail the test by panicking; the report goes to the test log"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_doctor::{
    AzCliFacts, BootFacts, CodeIntegrityFacts, DataEncryption, DataPathFacts, DiskEncryptionFacts,
    DiskFacts, GsaFacts, HypervisorFacts, JobFacts, Os, Probe, ProcessOutcome, Protection,
    RuntimeFacts, SshAgentFacts, SwapArea, SwapFacts,
};
use puddle_doctor::{CheckId, Finding, Options, Report, Status, SystemProbe, diagnose};
use puddle_runtime::{
    DevOverride, HostOs, MSB_FILE_NAME, RUNTIME_DIR_NAME, RuntimeLayout, RuntimeVersion,
};
use puddle_vm_tests::{RuntimePair, Settings};

/// The acceptance bar for a whole run.
const BAR: Duration = Duration::from_secs(30);

fn settings() -> Settings {
    Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM settings")
}

/// The runtime pair CI installed (`PUDDLE_VM_RUNTIME_DIR`).
fn ci_runtime() -> RuntimePair {
    RuntimePair::find_in(&settings().runtime_dir).expect("runtime pair")
}

/// The real probe with two facts of the machine the test runs on fixed: its disk encryption
/// and its Azure CLI. Whether a CI runner's disk and swap are encrypted says nothing about the
/// runtime, the test boot or the paths this test checks, and the hosted runners are not
/// encrypted, so the "every check ok or info" bar below would depend on the runner image.
/// The same goes for `az`: the real one is a slow Python CLI that a cold, busy runner can take
/// over the doctor's 10 s limit to answer, which made this test flake. The probe's
/// own answers on the real machine are tested in `puddle-doctor`'s `system_probe` tests.
struct EncryptedHost(SystemProbe);

impl Probe for EncryptedHost {
    fn os(&self) -> Os {
        self.0.os()
    }
    fn arch(&self) -> String {
        self.0.arch()
    }
    fn hypervisor(&self) -> HypervisorFacts {
        self.0.hypervisor()
    }
    fn code_integrity(&self) -> Option<CodeIntegrityFacts> {
        self.0.code_integrity()
    }
    fn runtime(&self) -> RuntimeFacts {
        self.0.runtime()
    }
    fn launch(&self, limit: Duration) -> ProcessOutcome {
        self.0.launch(limit)
    }
    fn boot(&self, limit: Duration) -> BootFacts {
        self.0.boot(limit)
    }
    fn job(&self) -> Option<JobFacts> {
        self.0.job()
    }
    fn global_secure_access(&self) -> Option<GsaFacts> {
        self.0.global_secure_access()
    }
    fn disk_space(&self) -> DiskFacts {
        self.0.disk_space()
    }
    fn data_path(&self) -> Option<DataPathFacts> {
        self.0.data_path()
    }
    fn disk_encryption(&self) -> DiskEncryptionFacts {
        let real = self.0.disk_encryption();
        let DataEncryption::Volume { dir, .. } = real.data else {
            return real;
        };
        let encrypted = Protection::Encrypted {
            by: "fixed for this test".into(),
        };
        DiskEncryptionFacts {
            data: DataEncryption::Volume {
                dir,
                protection: encrypted.clone(),
            },
            swap: SwapFacts::Areas(vec![SwapArea {
                name: "fixed for this test".into(),
                protection: encrypted,
            }]),
        }
    }
    fn ssh_agent(&self, limit: Duration) -> SshAgentFacts {
        self.0.ssh_agent(limit)
    }
    fn az_cli(&self, _limit: Duration) -> AzCliFacts {
        AzCliFacts::Found {
            version: "fixed for this test".into(),
        }
    }
}

/// A puddle-style runtime folder in `root` with a copy (never a link: the permission test
/// changes the file) of CI's pair, laid out as Puddle ships it.
fn runtime_copy(root: &Path) -> PathBuf {
    let pair = ci_runtime();
    let dir = root.join(RUNTIME_DIR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(&pair.msb, dir.join(MSB_FILE_NAME)).unwrap();
    // msb pairs itself with the firmware beside it, under the name it was released with, so copy
    // every firmware name the runtime folder holds (`libkrunfw.so.5` and the versioned file).
    let prefix = HostOs::current().runtime_files().libkrunfw_prefix;
    for entry in std::fs::read_dir(pair.msb.parent().unwrap())
        .unwrap()
        .flatten()
    {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(prefix) && entry.path().is_file() {
            std::fs::copy(entry.path(), dir.join(&name)).unwrap();
        }
    }
    assert_firmware_names(&dir);
    dir
}

/// The folder holds every firmware name msb can look for, so a runtime laid out without one
/// (`libkrunfw.so.5` or, on Linux, the versioned copy) fails here and not as an "incomplete
/// runtime" at the first boot. The names come from the runtime table, as the doctor's own check.
fn assert_firmware_names(dir: &Path) {
    let layout = RuntimeLayout::new(dir.to_owned(), dir.join("home")).unwrap();
    for file in layout
        .required_files()
        .iter()
        .chain([&layout.libkrunfw_path()])
    {
        assert!(file.is_file(), "{} is missing", file.display());
    }
    let os = HostOs::current();
    // Only a platform with a versioned firmware name (Linux) has a second one to look for.
    if os.libkrunfw_versioned("0").is_some() {
        let installed = os.runtime_files().libkrunfw;
        let prefix = format!("{}.", os.runtime_files().libkrunfw_prefix);
        let versioned = std::fs::read_dir(dir).unwrap().flatten().any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name != installed
                && name
                    .strip_prefix(&prefix)
                    .is_some_and(|version| os.libkrunfw_versioned(version) == Some(name.clone()))
        });
        assert!(
            versioned,
            "{} has no firmware under a versioned name ({prefix}<version>) beside {installed}",
            dir.display()
        );
    }
}

fn run(dir: PathBuf, expected: RuntimeVersion, options: &Options) -> Report {
    // A failed test boot's msb logs land beside the other kept sandbox logs (the CI artifact).
    // The data folder sits beside the runtime folder, under the test's tempdir, so the Linux
    // data-folder check runs on a real folder instead of reporting that none was given.
    let data = dir.parent().unwrap().join("data");
    std::fs::create_dir_all(&data).unwrap();
    // A host with umask 002 makes the tempdir group-writable, which the check rightly flags.
    #[cfg(unix)]
    for folder in [&data, data.parent().unwrap()] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let probe = EncryptedHost(
        SystemProbe::new(dir, expected, DevOverride::none())
            .with_data_dir(data)
            .with_keep_boot_logs(settings().kept_logs()),
    );
    let report = diagnose(&probe, options, "vm-test");
    eprintln!("{}", report.to_text());
    report
}

fn status(report: &Report, id: CheckId) -> Status {
    report.check(id).map_or(Status::Skipped, |c| c.status)
}

fn finding(report: &Report, id: CheckId) -> Option<Finding> {
    report.check(id).and_then(|c| c.finding)
}

#[test]
fn vm_doctor_is_all_green_with_a_test_boot_under_30_s() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_copy(root.path());
    let report = run(dir, RuntimeVersion::built_for(), &Options::default());
    for id in [
        CheckId::Hypervisor,
        CheckId::Runtime,
        CheckId::Launch,
        CheckId::TestBoot,
    ] {
        assert_eq!(status(&report, id), Status::Ok, "{id:?}");
    }
    if cfg!(target_os = "linux") {
        assert_eq!(status(&report, CheckId::DataPath), Status::Ok);
    }
    assert!(
        report
            .checks
            .iter()
            .all(|c| matches!(c.status, Status::Ok | Status::Info)),
        "every check ok or info"
    );
    assert!(report.ok);
    assert!(
        Duration::from_millis(report.elapsed_ms) < BAR,
        "{} ms",
        report.elapsed_ms
    );
}

/// msb's boot race is fixed in the fork, so the doctor's test boot has no retry: every one
/// of 20 boots in a row must pass on its own.
#[test]
fn vm_test_boot_passes_20_in_a_row_without_a_retry() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_copy(root.path());
    let kept = settings().kept_logs();
    for round in 1..=20 {
        let facts = puddle_doctor::boot::test_boot(&dir, "x86_64", BAR, Some(&kept));
        let ok = matches!(
            &facts,
            BootFacts::Ran {
                outcome: ProcessOutcome::Exited {
                    code: Some(puddle_doctor::PROBE_EXIT_CODE),
                    ..
                },
                ..
            }
        );
        assert!(
            ok,
            "boot {round} of 20: {facts:?} (msb's logs kept in {})",
            kept.display()
        );
    }
}

#[test]
fn vm_doctor_names_a_missing_runtime() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(RUNTIME_DIR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    let report = run(dir, RuntimeVersion::built_for(), &Options::default());
    assert_eq!(status(&report, CheckId::Hypervisor), Status::Ok);
    assert_eq!(
        finding(&report, CheckId::Runtime),
        Some(Finding::RuntimeMissing)
    );
    assert_eq!(status(&report, CheckId::Launch), Status::Skipped);
    assert_eq!(status(&report, CheckId::TestBoot), Status::Skipped);
    assert!(!report.ok);
}

#[test]
fn vm_doctor_names_both_versions_on_a_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_copy(root.path());
    let built = RuntimeVersion::built_for();
    let other: RuntimeVersion =
        format!("{}-puddle.{}", built.release(), built.puddle_revision() + 1)
            .parse()
            .unwrap();
    let report = run(dir, other, &Options::default());
    assert_eq!(
        finding(&report, CheckId::Runtime),
        Some(Finding::RuntimeVersionMismatch)
    );
    let summary = &report.check(CheckId::Runtime).unwrap().summary;
    assert!(summary.contains(&other.to_string()), "{summary}");
    assert!(summary.contains(&built.to_string()), "{summary}");
    assert_eq!(status(&report, CheckId::TestBoot), Status::Skipped);
}

#[test]
fn vm_doctor_reports_a_runtime_this_user_may_not_run() {
    let root = tempfile::tempdir().unwrap();
    let dir = runtime_copy(root.path());
    let msb = dir.join(MSB_FILE_NAME);
    let _restore = deny_run(&msb);
    let report = run(dir, RuntimeVersion::built_for(), &Options::default());
    let runtime = report.check(CheckId::Runtime).unwrap();
    assert_eq!(
        runtime.finding,
        Some(Finding::RuntimePermissions),
        "{runtime:?}"
    );
    let fix = runtime.fix.as_deref().unwrap();
    assert!(fix.contains(&*msb.to_string_lossy()), "{fix}");
    assert_eq!(status(&report, CheckId::Launch), Status::Skipped);
}

/// A real folder above the data folder that another user owns: the doctor names the folder and
/// the `chown` that fixes it. Needs root (or passwordless sudo) to hand a folder to another
/// uid, so a host without either says so and skips; the hosted runners have sudo.
#[cfg(target_os = "linux")]
#[test]
fn vm_doctor_names_a_folder_above_the_data_folder_that_another_user_owns() {
    use std::os::unix::fs::MetadataExt;

    const OTHER_UID: u32 = 4242;
    let root = tempfile::tempdir().unwrap();
    let mine = std::fs::metadata(root.path()).unwrap().uid();
    let foreign = root.path().join("foreign");
    let data = foreign.join("data");
    std::fs::create_dir_all(&data).unwrap();
    for folder in [root.path(), &foreign, &data] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let Some(_restore) = ChownGuard::give(&foreign, OTHER_UID, mine) else {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_CHOWN").is_none(),
            "PUDDLE_REQUIRE_CHOWN is set but neither root nor passwordless sudo can chown"
        );
        eprintln!("skipped: no root or passwordless sudo to chown with");
        return;
    };
    let dir = root.path().join(RUNTIME_DIR_NAME);
    std::fs::create_dir_all(&dir).unwrap();
    let probe =
        SystemProbe::new(dir, RuntimeVersion::built_for(), DevOverride::none()).with_data_dir(data);
    let report = diagnose(&probe, &Options::default(), "vm-test");
    eprintln!("{}", report.to_text());
    let check = report.check(CheckId::DataPath).unwrap();
    assert_eq!(check.finding, Some(Finding::DataPathBlocked), "{check:?}");
    let named = std::fs::canonicalize(&foreign).unwrap();
    let named = named.to_string_lossy();
    assert!(check.summary.contains(&*named), "{}", check.summary);
    assert!(check.summary.contains("4242"), "{}", check.summary);
    let fix = check.fix.as_deref().unwrap();
    assert!(fix.contains(&*named) && fix.contains("chown"), "{fix}");
    assert!(!report.ok);
}

/// Hands a folder to another uid and, on drop, back to its owner so the temp dir can be removed.
#[cfg(target_os = "linux")]
struct ChownGuard {
    path: PathBuf,
    back_to: u32,
}

#[cfg(target_os = "linux")]
impl ChownGuard {
    /// `None` when this account may not change owners.
    fn give(path: &Path, uid: u32, back_to: u32) -> Option<Self> {
        Self::chown(path, uid).then(|| Self {
            path: path.to_owned(),
            back_to,
        })
    }

    fn chown(path: &Path, uid: u32) -> bool {
        let uid = uid.to_string();
        let direct = std::process::Command::new("chown")
            .arg(&uid)
            .arg(path)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        direct
            || std::process::Command::new("sudo")
                .args(["-n", "chown", &uid])
                .arg(path)
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    }
}

#[cfg(target_os = "linux")]
impl Drop for ChownGuard {
    fn drop(&mut self) {
        assert!(
            Self::chown(&self.path, self.back_to),
            "restoring {}",
            self.path.display()
        );
    }
}

/// Undoes [`deny_run`] on drop, so the temp dir can be removed.
struct Restore {
    #[cfg(windows)]
    path: PathBuf,
    #[cfg(windows)]
    sid: String,
}

impl Drop for Restore {
    fn drop(&mut self) {
        #[cfg(windows)]
        icacls(&self.path, &["/remove:d", &format!("*{}", self.sid)]);
    }
}

/// Takes away this user's right to run `path`: a deny ACE for read + execute.
#[cfg(windows)]
fn deny_run(path: &Path) -> Restore {
    let sid = current_user_sid();
    icacls(path, &["/deny", &format!("*{sid}:(RX)")]);
    Restore {
        path: path.to_owned(),
        sid,
    }
}

/// Takes away the execute bits of `path`.
#[cfg(unix)]
fn deny_run(path: &Path) -> Restore {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    Restore {}
}

#[cfg(windows)]
fn icacls(path: &Path, args: &[&str]) {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    eprintln!(
        "icacls {args:?}: {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
    assert!(out.status.success(), "icacls {args:?}: {out:?}");
}

/// This user's SID: the `S-1-...` field of `whoami /user`.
#[cfg(windows)]
fn current_user_sid() -> String {
    let out = std::process::Command::new("whoami")
        .arg("/user")
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.split_whitespace()
        .rev()
        .find(|field| field.starts_with("S-1-"))
        .expect("a SID in the output of whoami /user")
        .to_owned()
}
