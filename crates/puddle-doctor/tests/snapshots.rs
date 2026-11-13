// SPDX-License-Identifier: GPL-3.0-or-later
//! Every finding, worded exactly: the report for each faked machine is compared with a file in
//! `tests/snapshots/` (text, then JSON). After a deliberate wording change, regenerate with
//! `PUDDLE_UPDATE_SNAPSHOTS=1 cargo nextest run -p puddle-doctor --test snapshots` and review
//! the diff.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

use puddle_doctor::{
    AZ_CLI_LIMIT, AccessDenied, AgentService, AzCliFacts, BlockedBy, BlockedFolder, BootFacts,
    CheckId, CodeIntegrity, CodeIntegrityFacts, DataEncryption, DataPathFacts, DiskEncryptionFacts,
    DiskFacts, Finding, GsaFacts, HypervisorApi, HypervisorFacts, JobFacts, KeptLogs, Options, Os,
    Probe, ProcessOutcome, Protection, Report, RuntimeFacts, RuntimeState, SshAgentFacts, Status,
    StoreHealth, SwapArea, SwapFacts, WritableFolder, diagnose,
};
use puddle_runtime::RuntimeError;

const DIR: &str = r"C:\Users\dev\AppData\Local\Programs\puddle\runtime";
const MSB: &str = r"C:\Users\dev\AppData\Local\Programs\puddle\runtime\msb.exe";
/// The runtime folder of the non-Windows machines (a Linux package puts it next to the program).
const DATA: &str = "/home/dev/.local/share/puddle";
const UNIX_DIR: &str = "/opt/puddle/runtime";
const UNIX_MSB: &str = "/opt/puddle/runtime/msb";
const WIN_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
const SOCK: &str = "/run/user/1000/agent.sock";
const EXPECTED: &str = "0.7.7-puddle.2";

/// A machine described by its facts; counts the calls that cost time.
#[derive(Clone)]
struct Fake {
    os: Os,
    hv: HypervisorFacts,
    ci: Option<CodeIntegrityFacts>,
    runtime: RuntimeState,
    launch: ProcessOutcome,
    boot: BootFacts,
    job: Option<JobFacts>,
    gsa: Option<GsaFacts>,
    disk: DiskFacts,
    data_path: Option<DataPathFacts>,
    enc: DiskEncryptionFacts,
    ssh: SshAgentFacts,
    az: AzCliFacts,
    store: StoreHealth,
    launches: Cell<u32>,
    boots: Cell<u32>,
}

fn exited(code: i32, stdout: &str, stderr: &str, ms: u64) -> ProcessOutcome {
    ProcessOutcome::Exited {
        code: Some(code),
        stdout_first_line: stdout.into(),
        stderr_tail: stderr.into(),
        elapsed: Duration::from_millis(ms),
    }
}

fn booted() -> BootFacts {
    BootFacts::Ran {
        outcome: exited(42, "", "", 1130),
        kept_logs: None,
    }
}

fn status(code: u32) -> i32 {
    i32::from_ne_bytes(code.to_ne_bytes())
}

/// A data folder whose drive has `gb` GB (1024 x 1024 x 1024 bytes) free.
fn disk_with_gb(gb: u64) -> DiskFacts {
    disk_with_bytes(gb * 1024 * 1024 * 1024)
}

fn disk_with_bytes(free_bytes: u64) -> DiskFacts {
    DiskFacts::Read {
        dir: PathBuf::from(DATA),
        free_bytes,
    }
}

fn encrypted(by: &str) -> DiskEncryptionFacts {
    let p = Protection::Encrypted { by: by.into() };
    encryption(p.clone(), p)
}

fn encryption(data: Protection, swap: Protection) -> DiskEncryptionFacts {
    DiskEncryptionFacts {
        data: DataEncryption::Volume {
            dir: PathBuf::from(DATA),
            protection: data,
        },
        swap: SwapFacts::Areas(vec![SwapArea {
            name: "/dev/dm-1".into(),
            protection: swap,
        }]),
    }
}

/// A healthy Windows workstation.
fn windows() -> Fake {
    Fake {
        os: Os::Windows,
        hv: HypervisorFacts {
            api: HypervisorApi::Ready,
            firmware_virtualization: Some(true),
            hypervisor_vendor: Some("Microsoft Hv".into()),
        },
        ci: Some(CodeIntegrityFacts::Read(CodeIntegrity {
            hvci: true,
            user_mode_enforced: false,
            user_mode_audit: false,
            test_signing: false,
        })),
        runtime: RuntimeState::Ready {
            version: EXPECTED.into(),
            overridden: false,
        },
        launch: exited(0, "msb 0.7.7-puddle.2", "", 85),
        boot: booted(),
        job: None,
        gsa: Some(GsaFacts::NotInstalled),
        disk: disk_with_gb(120),
        data_path: None,
        enc: encrypted("BitLocker"),
        ssh: SshAgentFacts::Keys {
            endpoint: WIN_PIPE.into(),
            families: vec!["ed25519".into(), "rsa".into()],
        },
        store: StoreHealth::Working,
        az: AzCliFacts::Found {
            version: "2.64.0".into(),
        },
        launches: Cell::new(0),
        boots: Cell::new(0),
    }
}

fn linux() -> Fake {
    Fake {
        os: Os::Linux,
        hv: HypervisorFacts {
            api: HypervisorApi::Ready,
            firmware_virtualization: Some(true),
            hypervisor_vendor: None,
        },
        ci: None,
        gsa: None,
        ssh: SshAgentFacts::Keys {
            endpoint: SOCK.into(),
            families: vec!["ed25519".into()],
        },
        ..windows()
    }
}

impl Probe for Fake {
    fn os(&self) -> Os {
        self.os
    }
    fn arch(&self) -> String {
        "x86_64".into()
    }
    fn hypervisor(&self) -> HypervisorFacts {
        self.hv.clone()
    }
    fn code_integrity(&self) -> Option<CodeIntegrityFacts> {
        self.ci.clone()
    }
    fn runtime(&self) -> RuntimeFacts {
        let (dir, msb) = if self.os == Os::Windows {
            (DIR, MSB)
        } else {
            (UNIX_DIR, UNIX_MSB)
        };
        RuntimeFacts {
            dir: PathBuf::from(dir),
            msb: PathBuf::from(msb),
            expected: EXPECTED.into(),
            state: self.runtime.clone(),
        }
    }
    fn launch(&self, _limit: Duration) -> ProcessOutcome {
        self.launches.set(self.launches.get() + 1);
        self.launch.clone()
    }
    fn boot(&self, limit: Duration) -> BootFacts {
        // flake-ok: no clock is read; this checks the limit the doctor hands to `boot`, a constant.
        assert!(limit >= Duration::from_secs(5) && limit <= Duration::from_secs(25));
        self.boots.set(self.boots.get() + 1);
        self.boot.clone()
    }
    fn job(&self) -> Option<JobFacts> {
        self.job.clone()
    }
    fn global_secure_access(&self) -> Option<GsaFacts> {
        self.gsa.clone()
    }
    fn disk_space(&self) -> DiskFacts {
        self.disk.clone()
    }
    fn data_path(&self) -> Option<DataPathFacts> {
        self.data_path.clone()
    }
    fn disk_encryption(&self) -> DiskEncryptionFacts {
        self.enc.clone()
    }
    fn ssh_agent(&self, limit: Duration) -> SshAgentFacts {
        assert_eq!(limit, Duration::from_secs(2), "the agent gets 2 s");
        self.ssh.clone()
    }
    fn az_cli(&self, limit: Duration) -> AzCliFacts {
        assert_eq!(limit, AZ_CLI_LIMIT, "the Azure CLI gets its own limit");
        self.az.clone()
    }
    fn credential_store(&self) -> StoreHealth {
        self.store
    }
}

fn run(fake: &Fake, options: &Options) -> Report {
    let mut report = diagnose(fake, options, "0.1.0");
    report.elapsed_ms = 1234;
    report
}

/// Compares (or with `PUDDLE_UPDATE_SNAPSHOTS=1`, writes) `tests/snapshots/<name>.snap`.
fn snapshot(name: &str, report: &Report) {
    let actual = format!("{}\n---\n{}\n", report.to_text(), report.to_json());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/snapshots")
        .join(format!("{name}.snap"));
    if std::env::var_os("PUDDLE_UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}; run with PUDDLE_UPDATE_SNAPSHOTS=1",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "snapshot {name} differs; run with PUDDLE_UPDATE_SNAPSHOTS=1 and review.\n--- expected\n{expected}\n--- actual\n{actual}"
    );
}

fn finding(report: &Report, id: CheckId) -> Option<Finding> {
    report.check(id).and_then(|c| c.finding)
}

fn check(name: &str, fake: &Fake, id: CheckId, expected: Option<Finding>) -> Report {
    let report = run(fake, &Options::default());
    snapshot(name, &report);
    assert_eq!(finding(&report, id), expected, "{name}");
    report
}

#[test]
fn windows_all_green() {
    let fake = windows();
    let r = check("windows_all_green", &fake, CheckId::TestBoot, None);
    assert!(r.ok);
    assert!(
        r.checks
            .iter()
            .all(|c| matches!(c.status, Status::Ok | Status::Info))
    );
    assert_eq!((fake.launches.get(), fake.boots.get()), (1, 1));
}

#[test]
fn windows_whp_not_enabled() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::NotInstalled {
                detail:
                    "WinHvPlatform.dll: The specified module could not be found. (os error 126)"
                        .into(),
            },
            firmware_virtualization: Some(true),
            hypervisor_vendor: None,
        },
        // msb links WinHvPlatform.dll, so it can't start either.
        launch: exited(status(0xC000_0135), "", "", 30),
        ..windows()
    };
    let r = check(
        "windows_whp_not_enabled",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::WhpNotEnabled),
    );
    assert_eq!(finding(&r, CheckId::Launch), Some(Finding::DllMissing));
    assert_eq!(fake.boots.get(), 0);
}

#[test]
fn windows_firmware_virtualization_off() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::NotRunning,
            firmware_virtualization: Some(false),
            hypervisor_vendor: None,
        },
        ..windows()
    };
    let r = check(
        "windows_firmware_off",
        &fake,
        CheckId::Virtualization,
        Some(Finding::FirmwareVirtualizationOff),
    );
    assert_eq!(
        finding(&r, CheckId::Hypervisor),
        Some(Finding::FirmwareVirtualizationOff)
    );
}

#[test]
fn windows_hypervisor_not_running() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::NotRunning,
            firmware_virtualization: Some(true),
            hypervisor_vendor: None,
        },
        ..windows()
    };
    check(
        "windows_hypervisor_not_running",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::HypervisorNotRunning),
    );
}

#[test]
fn windows_in_a_vm_without_nested_virtualization() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::NotRunning,
            firmware_virtualization: Some(false),
            hypervisor_vendor: Some("VMwareVMware".into()),
        },
        ..windows()
    };
    let r = check(
        "windows_vm_without_nesting",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::NestedVirtualizationOff),
    );
    assert_eq!(
        r.check(CheckId::Virtualization).unwrap().status,
        Status::Info
    );
}

#[test]
fn windows_hypervisor_query_failed() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::QueryFailed {
                detail: "WHvGetCapability failed: HRESULT 0x80070005".into(),
            },
            firmware_virtualization: None,
            hypervisor_vendor: None,
        },
        ..windows()
    };
    check(
        "windows_hypervisor_query_failed",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::HypervisorQueryFailed),
    );
}

#[test]
fn windows_runtime_missing() {
    let fake = Fake {
        runtime: RuntimeState::Unusable(RuntimeError::Missing { path: MSB.into() }),
        ..windows()
    };
    check(
        "windows_runtime_missing",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimeMissing),
    );
    assert_eq!((fake.launches.get(), fake.boots.get()), (0, 0));
}

#[test]
fn windows_runtime_version_mismatch() {
    let fake = Fake {
        runtime: RuntimeState::Unusable(RuntimeError::Mismatch {
            path: MSB.into(),
            expected: EXPECTED.into(),
            found: "0.7.7".into(),
        }),
        ..windows()
    };
    check(
        "windows_runtime_version_mismatch",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimeVersionMismatch),
    );
    let no_version = Fake {
        runtime: RuntimeState::Unusable(RuntimeError::NoVersion {
            path: MSB.into(),
            expected: EXPECTED.into(),
        }),
        ..windows()
    };
    let r = run(&no_version, &Options::default());
    assert_eq!(
        finding(&r, CheckId::Runtime),
        Some(Finding::RuntimeVersionMismatch)
    );
}

#[test]
fn windows_runtime_unreadable() {
    let fake = Fake {
        runtime: RuntimeState::Unusable(RuntimeError::Unreadable {
            path: MSB.into(),
            reason: "not an executable".into(),
        }),
        ..windows()
    };
    check(
        "windows_runtime_unreadable",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimeUnreadable),
    );
}

#[test]
fn windows_runtime_permissions() {
    let fake = Fake {
        runtime: RuntimeState::AccessDenied(AccessDenied {
            path: MSB.into(),
            detail: "Access is denied. (os error 5)".into(),
        }),
        ..windows()
    };
    check(
        "windows_runtime_permissions",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimePermissions),
    );
}

#[test]
fn windows_runtime_dev_override_warns() {
    let fake = Fake {
        runtime: RuntimeState::Ready {
            version: "0.7.8".into(),
            overridden: true,
        },
        ..windows()
    };
    let r = check(
        "windows_runtime_overridden",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimeOverridden),
    );
    assert!(r.ok);
}

fn spawn_failed(os_error: i32, detail: &str) -> ProcessOutcome {
    ProcessOutcome::SpawnFailed {
        os_error: Some(os_error),
        detail: format!("{detail} (os error {os_error})"),
    }
}

#[test]
fn windows_blocked_by_applocker() {
    let fake = Fake {
        launch: spawn_failed(
            1260,
            "This program is blocked by group policy. For more information, contact your system administrator.",
        ),
        ..windows()
    };
    check(
        "windows_applocker",
        &fake,
        CheckId::Launch,
        Some(Finding::BlockedByAppLocker),
    );
    assert_eq!(fake.boots.get(), 0);
}

#[test]
fn windows_blocked_by_app_control() {
    let fake = Fake {
        ci: Some(CodeIntegrityFacts::Read(CodeIntegrity {
            hvci: true,
            user_mode_enforced: true,
            user_mode_audit: false,
            test_signing: false,
        })),
        launch: spawn_failed(
            4551,
            "Your organization used Device Guard to block this app. Contact your support person for more info.",
        ),
        ..windows()
    };
    check(
        "windows_app_control",
        &fake,
        CheckId::Launch,
        Some(Finding::BlockedByAppControl),
    );
}

#[test]
fn windows_blocked_by_smart_app_control() {
    let fake = Fake {
        launch: spawn_failed(4580, "An Application Control policy has blocked this file."),
        ..windows()
    };
    check(
        "windows_smart_app_control",
        &fake,
        CheckId::Launch,
        Some(Finding::BlockedByAppControl),
    );
}

#[test]
fn windows_blocked_by_antivirus() {
    let fake = Fake {
        launch: spawn_failed(
            225,
            "Operation did not complete successfully because the file contains a virus or potentially unwanted software.",
        ),
        ..windows()
    };
    check(
        "windows_antivirus",
        &fake,
        CheckId::Launch,
        Some(Finding::BlockedByAntivirus),
    );
}

#[test]
fn windows_process_creation_denied_is_edr() {
    let fake = Fake {
        launch: spawn_failed(5, "Access is denied."),
        ..windows()
    };
    check(
        "windows_edr",
        &fake,
        CheckId::Launch,
        Some(Finding::ProcessCreationDenied),
    );
}

#[test]
fn windows_launch_failed_otherwise() {
    let fake = Fake {
        launch: spawn_failed(193, "%1 is not a valid Win32 application."),
        ..windows()
    };
    check(
        "windows_launch_failed",
        &fake,
        CheckId::Launch,
        Some(Finding::LaunchFailed),
    );
}

#[test]
fn windows_launch_crashed() {
    let fake = Fake {
        launch: exited(status(0xC000_0005), "", "", 40),
        ..windows()
    };
    check(
        "windows_launch_crashed",
        &fake,
        CheckId::Launch,
        Some(Finding::LaunchExited),
    );
}

#[test]
fn windows_launch_timed_out() {
    let fake = Fake {
        launch: ProcessOutcome::TimedOut {
            after: Duration::from_secs(10),
        },
        ..windows()
    };
    check(
        "windows_launch_timeout",
        &fake,
        CheckId::Launch,
        Some(Finding::LaunchTimedOut),
    );
    assert_eq!(fake.boots.get(), 0);
}

#[test]
fn windows_boot_failed() {
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: exited(
                1,
                "",
                "error: failed to start sandbox\n  → sandbox process exited (exit code: 0) before agent relay became available",
                10_400,
            ),
            kept_logs: None,
        },
        ..windows()
    };
    check(
        "windows_boot_failed",
        &fake,
        CheckId::TestBoot,
        Some(Finding::BootFailed),
    );
}

#[test]
fn windows_boot_timed_out() {
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: ProcessOutcome::TimedOut {
                after: Duration::from_secs(25),
            },
            kept_logs: None,
        },
        ..windows()
    };
    check(
        "windows_boot_timeout",
        &fake,
        CheckId::TestBoot,
        Some(Finding::BootTimedOut),
    );
}

const DENIED: &str = "error: io error: Access is denied. (os error 5)";

#[test]
fn windows_boot_denied_in_a_closed_job_is_the_job_not_edr() {
    let fake = Fake {
        job: Some(JobFacts::Read {
            breakaway_allowed: false,
        }),
        boot: BootFacts::Ran {
            outcome: exited(1, "", DENIED, 900),
            kept_logs: None,
        },
        ..windows()
    };
    let r = check(
        "windows_boot_job",
        &fake,
        CheckId::TestBoot,
        Some(Finding::BootBlockedByJob),
    );
    assert_eq!(
        finding(&r, CheckId::JobObject),
        Some(Finding::JobWithoutBreakaway)
    );
    // The same refusal outside such a job points at endpoint security.
    for job in [
        None,
        Some(JobFacts::Read {
            breakaway_allowed: true,
        }),
    ] {
        let fake = Fake {
            job,
            ..fake.clone()
        };
        let r = run(&fake, &Options::default());
        assert_eq!(
            finding(&r, CheckId::TestBoot),
            Some(Finding::ProcessCreationDenied)
        );
        assert!(r.check(CheckId::JobObject).is_none());
    }
}

#[test]
fn windows_job_unreadable_is_said_not_dropped() {
    let fake = Fake {
        job: Some(JobFacts::query_failed("QueryInformationJobObject", 87)),
        ..windows()
    };
    let r = check(
        "windows_job_unreadable",
        &fake,
        CheckId::JobObject,
        Some(Finding::JobUnreadable),
    );
    assert_eq!(r.check(CheckId::JobObject).unwrap().status, Status::Info);
}

#[test]
fn windows_boot_spawn_refused() {
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: spawn_failed(1260, "This program is blocked by group policy."),
            kept_logs: None,
        },
        ..windows()
    };
    let r = run(&fake, &Options::default());
    assert_eq!(
        finding(&r, CheckId::TestBoot),
        Some(Finding::BlockedByAppLocker)
    );
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: spawn_failed(1450, "Insufficient system resources exist."),
            kept_logs: None,
        },
        ..windows()
    };
    let r = run(&fake, &Options::default());
    assert_eq!(finding(&r, CheckId::TestBoot), Some(Finding::BootFailed));
}

#[test]
fn windows_boot_setup_failed() {
    let fake = Fake {
        boot: BootFacts::SetupFailed {
            detail: r"temp dir: Access is denied. (os error 5)".into(),
        },
        ..windows()
    };
    check(
        "windows_boot_setup_failed",
        &fake,
        CheckId::TestBoot,
        Some(Finding::BootSetupFailed),
    );
}

#[test]
fn windows_gsa_installed_warns_and_explains() {
    let fake = Fake {
        gsa: Some(GsaFacts::Installed {
            running: Some(true),
        }),
        ci: Some(CodeIntegrityFacts::Read(CodeIntegrity {
            hvci: false,
            user_mode_enforced: false,
            user_mode_audit: true,
            test_signing: true,
        })),
        ..windows()
    };
    let r = check(
        "windows_gsa_installed",
        &fake,
        CheckId::GlobalSecureAccess,
        Some(Finding::GlobalSecureAccessInstalled),
    );
    assert!(r.ok, "a warning doesn't fail the report");
    let stopped = Fake {
        gsa: Some(GsaFacts::Installed {
            running: Some(false),
        }),
        ..windows()
    };
    let r = run(&stopped, &Options::default());
    assert!(
        r.check(CheckId::GlobalSecureAccess)
            .unwrap()
            .summary
            .contains("not running")
    );
}

#[test]
fn windows_unreadable_code_integrity_is_an_info_check_not_a_missing_one() {
    let fake = Fake {
        ci: Some(CodeIntegrityFacts::query_failed(status(0xC000_0022))),
        ..windows()
    };
    let r = check(
        "windows_code_integrity_unreadable",
        &fake,
        CheckId::CodeIntegrity,
        Some(Finding::CodeIntegrityUnreadable),
    );
    let ci = r.check(CheckId::CodeIntegrity).unwrap();
    assert_eq!(ci.status, Status::Info);
    assert!(ci.summary.contains("could not be read"), "{ci:?}");
    assert!(
        ci.detail.as_deref().unwrap().contains("0xC0000022"),
        "{ci:?}"
    );
    assert!(r.ok);
}

#[test]
fn windows_gsa_that_cannot_be_asked_is_unknown_with_the_os_error() {
    let fake = Fake {
        gsa: Some(GsaFacts::Unknown {
            detail: "OpenSCManagerW: Access is denied. (os error 5)".into(),
        }),
        ..windows()
    };
    let r = check(
        "windows_gsa_unknown",
        &fake,
        CheckId::GlobalSecureAccess,
        Some(Finding::GlobalSecureAccessUnknown),
    );
    let gsa = r.check(CheckId::GlobalSecureAccess).unwrap();
    assert_eq!(gsa.status, Status::Info);
    assert!(
        gsa.detail.as_deref().unwrap().contains("os error 5"),
        "{gsa:?}"
    );
    assert!(r.ok);
}

#[test]
fn windows_gsa_with_an_unreadable_service_does_not_claim_not_running() {
    let fake = Fake {
        gsa: Some(GsaFacts::Installed { running: None }),
        ..windows()
    };
    let r = check(
        "windows_gsa_running_unknown",
        &fake,
        CheckId::GlobalSecureAccess,
        Some(Finding::GlobalSecureAccessInstalled),
    );
    let summary = &r.check(CheckId::GlobalSecureAccess).unwrap().summary;
    assert!(summary.contains("running state unknown"), "{summary}");
    assert!(!summary.contains("not running"), "{summary}");
}

#[test]
fn a_failed_boots_kept_logs_are_named_in_the_report() {
    let ran = |kept_logs| Fake {
        boot: BootFacts::Ran {
            outcome: exited(1, "", "error: no hypervisor", 900),
            kept_logs: Some(kept_logs),
        },
        ..linux()
    };
    let kept = ran(KeptLogs::Kept {
        // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
        dir: "/home/dev/kept/puddle-doctor-boot-1".into(),
        failed: vec![],
    });
    let r = check(
        "linux_boot_logs_kept",
        &kept,
        CheckId::TestBoot,
        Some(Finding::BootFailed),
    );
    let detail = r.check(CheckId::TestBoot).unwrap().detail.clone().unwrap();
    assert!(
        detail.contains("error: no hypervisor")
            && detail.contains("kept in /home/dev/kept/puddle-doctor-boot-1"),
        "{detail}"
    );
    let partial = ran(KeptLogs::Kept {
        // flake-ok: a value that is parsed or compared and never opened, so the missing drive letter does not matter
        dir: "/home/dev/kept/puddle-doctor-boot-1".into(),
        failed: vec!["runtime.log: Permission denied".into()],
    });
    let detail = run(&partial, &Options::default())
        .check(CheckId::TestBoot)
        .unwrap()
        .detail
        .clone()
        .unwrap();
    assert!(
        detail.contains("except runtime.log: Permission denied"),
        "{detail}"
    );
    let lost = ran(KeptLogs::NotKept {
        detail: "no folder to keep them in under /home/dev/kept: Permission denied".into(),
    });
    let r = check(
        "linux_boot_logs_not_kept",
        &lost,
        CheckId::TestBoot,
        Some(Finding::BootFailed),
    );
    let detail = r.check(CheckId::TestBoot).unwrap().detail.clone().unwrap();
    assert!(detail.contains("were not kept: no folder"), "{detail}");
}

#[test]
fn kept_logs_are_named_even_when_the_boot_failure_has_no_other_evidence() {
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: ProcessOutcome::TimedOut {
                after: Duration::from_secs(25),
            },
            kept_logs: Some(KeptLogs::Kept {
                dir: "/k/puddle-doctor-boot-2".into(),
                failed: vec![],
            }),
        },
        ..linux()
    };
    let r = run(&fake, &Options::default());
    assert_eq!(
        r.check(CheckId::TestBoot).unwrap().detail.as_deref(),
        Some("msb's logs of this boot are kept in /k/puddle-doctor-boot-2")
    );
}

#[test]
fn no_boot_skips_the_test_boot() {
    let fake = windows();
    let r = run(
        &fake,
        &Options {
            boot: false,
            ..Options::default()
        },
    );
    snapshot("windows_no_boot", &r);
    assert_eq!(r.check(CheckId::TestBoot).unwrap().status, Status::Skipped);
    assert_eq!(fake.boots.get(), 0);
}

#[test]
fn no_time_left_skips_the_test_boot() {
    let fake = windows();
    let r = run(
        &fake,
        &Options {
            budget: Duration::from_secs(1),
            ..Options::default()
        },
    );
    assert_eq!(
        r.check(CheckId::TestBoot).unwrap().summary,
        "not checked: out of time"
    );
    assert_eq!(fake.boots.get(), 0);
}

#[test]
fn unsupported_arch_skips_the_test_boot() {
    let fake = Fake {
        boot: BootFacts::UnsupportedArch {
            arch: "aarch64".into(),
        },
        ..windows()
    };
    let r = run(&fake, &Options::default());
    let c = r.check(CheckId::TestBoot).unwrap();
    assert_eq!(c.status, Status::Skipped);
    assert!(c.summary.contains("aarch64"));
}

#[test]
fn linux_all_green() {
    let r = check("linux_all_green", &linux(), CheckId::Hypervisor, None);
    assert!(r.check(CheckId::CodeIntegrity).is_none());
    assert!(r.check(CheckId::GlobalSecureAccess).is_none());
}

#[test]
fn linux_kvm_missing() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::NotInstalled {
                detail: "/dev/kvm: No such file or directory (os error 2)".into(),
            },
            firmware_virtualization: Some(false),
            hypervisor_vendor: Some("Microsoft Hv".into()),
        },
        ..linux()
    };
    check(
        "linux_kvm_missing",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::KvmMissing),
    );
}

#[test]
fn linux_kvm_access_denied() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::AccessDenied {
                detail: "/dev/kvm: Permission denied (os error 13)".into(),
            },
            firmware_virtualization: Some(true),
            hypervisor_vendor: None,
        },
        launch: spawn_failed(13, "Permission denied"),
        ..linux()
    };
    let r = check(
        "linux_kvm_access_denied",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::KvmAccessDenied),
    );
    assert_eq!(
        finding(&r, CheckId::Launch),
        Some(Finding::RuntimePermissions)
    );
}

#[test]
fn linux_query_failed_and_killed_by_signal() {
    let fake = Fake {
        hv: HypervisorFacts {
            api: HypervisorApi::QueryFailed {
                detail: "/dev/kvm: Is a directory (os error 21)".into(),
            },
            firmware_virtualization: None,
            hypervisor_vendor: None,
        },
        launch: ProcessOutcome::Exited {
            code: None,
            stdout_first_line: String::new(),
            stderr_tail: String::new(),
            elapsed: Duration::from_millis(5),
        },
        ..linux()
    };
    let r = check(
        "linux_query_failed",
        &fake,
        CheckId::Launch,
        Some(Finding::LaunchExited),
    );
    assert!(
        r.check(CheckId::Launch)
            .unwrap()
            .summary
            .contains("killed by a signal")
    );
}

#[test]
fn linux_boot_killed_by_signal_and_odd_exit() {
    for code in [None, Some(3)] {
        let fake = Fake {
            boot: BootFacts::Ran {
                outcome: ProcessOutcome::Exited {
                    code,
                    stdout_first_line: String::new(),
                    stderr_tail: "boom".into(),
                    elapsed: Duration::from_millis(5),
                },
                kept_logs: None,
            },
            ..linux()
        };
        let r = run(&fake, &Options::default());
        assert_eq!(finding(&r, CheckId::TestBoot), Some(Finding::BootFailed));
    }
}

#[test]
fn linux_runtime_permissions_say_chmod() {
    let fake = Fake {
        runtime: RuntimeState::AccessDenied(AccessDenied {
            path: "/opt/puddle/runtime/msb".into(),
            detail: "not executable".into(),
        }),
        ..linux()
    };
    let r = check(
        "linux_runtime_permissions",
        &fake,
        CheckId::Runtime,
        Some(Finding::RuntimePermissions),
    );
    let fix = r.check(CheckId::Runtime).unwrap().fix.clone().unwrap();
    assert!(fix.contains("chmod a+rx"), "{fix}");
    assert!(!fix.contains("icacls"), "{fix}");
}

#[test]
fn linux_boot_without_firmware_is_an_incomplete_runtime() {
    // What the fork's msb says when no libkrunfw.so.<version> sits beside it (K run 37429055402).
    let fake = Fake {
        boot: BootFacts::Ran {
            outcome: exited(
                1,
                "",
                "error: microsandbox runtime installation is incomplete: /opt/puddle/runtime/msb\nhas no matching libkrunfw",
                40,
            ),
            kept_logs: None,
        },
        ..linux()
    };
    check(
        "linux_boot_runtime_incomplete",
        &fake,
        CheckId::TestBoot,
        Some(Finding::RuntimeMissing),
    );
}

#[test]
fn other_os_has_no_hypervisor_support() {
    let fake = Fake {
        os: Os::Other,
        hv: HypervisorFacts {
            api: HypervisorApi::Unsupported,
            firmware_virtualization: None,
            hypervisor_vendor: None,
        },
        ci: None,
        gsa: None,
        ..windows()
    };
    check(
        "other_unsupported",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::UnsupportedOs),
    );
}

#[test]
fn macos_is_not_supported_yet() {
    let fake = Fake {
        os: Os::MacOs,
        hv: HypervisorFacts {
            api: HypervisorApi::Unsupported,
            firmware_virtualization: None,
            hypervisor_vendor: None,
        },
        ..linux()
    };
    let r = check(
        "macos_unsupported",
        &fake,
        CheckId::Hypervisor,
        Some(Finding::UnsupportedOs),
    );
    let hypervisor = r.check(CheckId::Hypervisor).unwrap();
    assert!(hypervisor.summary.contains("macOS yet"), "{hypervisor:?}");
    assert_eq!(r.os, "macos");
    assert!(!r.ok);
}

#[test]
fn json_schema_is_versioned_and_stable() {
    let r = run(&windows(), &Options::default());
    let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(v["schema_version"], 1);
    let ids: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "virtualization",
            "hypervisor",
            "code_integrity",
            "runtime",
            "launch",
            "test_boot",
            "disk_space",
            "guest_kernel",
            "disk_encryption",
            "ssh_agent",
            "az_cli",
            "credential_store",
            "global_secure_access"
        ]
    );
}

#[test]
fn the_windows_report_names_the_guest_kernels_dev_mem_and_the_linux_one_does_not() {
    let r = run(&windows(), &Options::default());
    let line = r.check(CheckId::GuestKernel).unwrap();
    assert_eq!(line.status, Status::Info);
    assert_eq!(line.finding, Some(Finding::GuestDevMemPresent));
    assert!(r.ok, "an info line must not fail the report");
    assert!(
        run(&linux(), &Options::default())
            .check(CheckId::GuestKernel)
            .is_none()
    );
}

const GB: u64 = 1024 * 1024 * 1024;

fn disk_check(name: &str, disk: DiskFacts, expected: Option<Finding>) -> Report {
    let fake = Fake { disk, ..linux() };
    check(name, &fake, CheckId::DiskSpace, expected)
}

#[test]
fn disk_with_room_is_ok() {
    let r = disk_check("disk_ok", disk_with_gb(37), None);
    assert!(r.ok);
    assert_eq!(r.check(CheckId::DiskSpace).unwrap().status, Status::Ok);
}

#[test]
fn disk_under_the_warning_line_warns_without_failing_the_report() {
    let r = disk_check("disk_low", disk_with_gb(12), Some(Finding::DiskSpaceLow));
    assert!(r.ok, "a warning doesn't fail the report");
    assert_eq!(r.check(CheckId::DiskSpace).unwrap().status, Status::Warn);
}

#[test]
fn disk_under_the_failure_line_fails_the_report() {
    let r = disk_check(
        "disk_critical",
        disk_with_gb(3),
        Some(Finding::DiskSpaceCritical),
    );
    assert!(!r.ok);
    assert_eq!(r.check(CheckId::DiskSpace).unwrap().status, Status::Fail);
}

#[test]
fn each_line_is_crossed_one_byte_under_it() {
    let at = |free| {
        run(
            &Fake {
                disk: disk_with_bytes(free),
                ..linux()
            },
            &Options::default(),
        )
    };
    let status = |r: &Report| r.check(CheckId::DiskSpace).unwrap().status;
    assert_eq!(status(&at(20 * GB)), Status::Ok);
    assert_eq!(status(&at(20 * GB - 1)), Status::Warn);
    assert_eq!(status(&at(5 * GB)), Status::Warn);
    assert_eq!(status(&at(5 * GB - 1)), Status::Fail);
    assert_eq!(status(&at(0)), Status::Fail);
}

#[test]
fn the_lines_come_from_the_options() {
    let options = Options {
        disk_warn_below: 50 * GB,
        disk_fail_below: 40 * GB,
        ..Options::default()
    };
    let at = |gb| {
        run(
            &Fake {
                disk: disk_with_gb(gb),
                ..linux()
            },
            &options,
        )
        .check(CheckId::DiskSpace)
        .unwrap()
        .status
    };
    assert_eq!(at(60), Status::Ok);
    assert_eq!(at(45), Status::Warn);
    assert_eq!(at(39), Status::Fail);
}

#[test]
fn disk_that_cannot_be_read_is_an_info_line_with_the_os_error() {
    let r = disk_check(
        "disk_unreadable",
        DiskFacts::Unreadable {
            dir: PathBuf::from(DATA),
            detail: "Permission denied (os error 13)".into(),
        },
        Some(Finding::DiskSpaceUnreadable),
    );
    assert!(r.ok);
    assert_eq!(r.check(CheckId::DiskSpace).unwrap().status, Status::Info);
}

#[test]
fn without_a_data_folder_the_disk_check_is_skipped() {
    let r = disk_check("disk_no_folder", DiskFacts::NoFolder, None);
    assert!(r.ok);
    assert_eq!(r.check(CheckId::DiskSpace).unwrap().status, Status::Skipped);
}

fn folder(path: &str, mode: u32, owned: bool) -> WritableFolder {
    WritableFolder {
        path: PathBuf::from(path),
        mode,
        owned,
    }
}

#[test]
fn linux_data_path_clear() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/home/dev/.local/share/puddle".into(), // flake-ok: a path the fake probe reports, never opened
            blocked: Vec::new(),
            writable: Vec::new(),
        }),
        ..linux()
    };
    let r = check("linux_data_path_clear", &fake, CheckId::DataPath, None);
    assert!(r.ok);
}

#[test]
fn linux_data_path_group_writable() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/home/dev/.local/share/puddle".into(), // flake-ok: a path the fake probe reports, never opened
            blocked: Vec::new(),
            writable: vec![folder("/home/dev/.local", 0o775, true)],
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_group_writable",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathWritable),
    );
    assert!(!r.ok);
    let c = r.check(CheckId::DataPath).unwrap();
    assert!(
        c.fix
            .as_deref()
            .unwrap()
            .contains("\n  chmod g-w /home/dev/.local")
    );
}

#[test]
fn linux_data_path_several_world_writable_and_not_owned() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/srv/my data/puddle".into(),
            blocked: Vec::new(),
            writable: vec![
                folder("/srv", 0o777, false),
                folder("/srv/my data", 0o775, true),
            ],
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_several",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathWritable),
    );
    let fix = r.check(CheckId::DataPath).unwrap().fix.clone().unwrap();
    assert!(
        fix.contains("\n  sudo chmod go-w /srv '/srv/my data'"),
        "{fix}"
    );
}

#[test]
fn linux_data_path_not_a_folder() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/home/dev/.local".into(), // flake-ok: a path the fake probe reports, never opened
            writable: Vec::new(),
            blocked: vec![BlockedFolder {
                path: "/home/dev/.local".into(), // flake-ok: a path the fake probe reports, never opened
                by: BlockedBy::NotAFolder,
            }],
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_not_a_folder",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathBlocked),
    );
    assert!(!r.ok);
}

#[test]
fn linux_data_path_other_owner_and_writable() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/srv/team/puddle".into(),
            writable: vec![folder("/srv/team", 0o775, false)],
            blocked: vec![BlockedFolder {
                path: "/srv/team".into(),
                by: BlockedBy::OtherOwner { uid: 1001 },
            }],
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_other_owner",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathBlocked),
    );
    let fix = r.check(CheckId::DataPath).unwrap().fix.clone().unwrap();
    assert!(fix.contains("sudo chown \"$USER\" /srv/team"), "{fix}");
    assert!(fix.contains("\n  sudo chmod g-w /srv/team"), "{fix}");
}

#[test]
fn linux_data_path_unreadable() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Unreadable {
            dir: "/home/dev/.local/share/puddle".into(), // flake-ok: a path the fake probe reports, never opened
            detail: "Permission denied (os error 13)".into(),
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_unreadable",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathUnreadable),
    );
    assert!(r.ok);
}

#[test]
fn linux_data_path_no_folder() {
    let fake = Fake {
        data_path: Some(DataPathFacts::NoFolder),
        ..linux()
    };
    let r = check("linux_data_path_no_folder", &fake, CheckId::DataPath, None);
    assert_eq!(r.check(CheckId::DataPath).unwrap().status, Status::Skipped);
}

#[test]
fn linux_data_path_world_writable_only() {
    let fake = Fake {
        data_path: Some(DataPathFacts::Checked {
            dir: "/data/puddle".into(),
            blocked: Vec::new(),
            writable: vec![folder("/data", 0o757, true)],
        }),
        ..linux()
    };
    let r = check(
        "linux_data_path_world_writable",
        &fake,
        CheckId::DataPath,
        Some(Finding::DataPathWritable),
    );
    let c = r.check(CheckId::DataPath).unwrap();
    assert!(c.summary.contains("writable by everyone"), "{}", c.summary);
    assert!(c.fix.as_deref().unwrap().contains("\n  chmod go-w /data"));
}

// ---- SSH agent ----

fn ssh_check(name: &str, fake: &Fake, expected: Option<Finding>) -> Report {
    let r = check(name, fake, CheckId::SshAgent, expected);
    assert!(r.ok, "the SSH agent never fails the report");
    r
}

fn windows_with(ssh: SshAgentFacts) -> Fake {
    Fake { ssh, ..windows() }
}

fn linux_with(ssh: SshAgentFacts) -> Fake {
    Fake { ssh, ..linux() }
}

fn unreachable(endpoint: &str, service: Option<AgentService>) -> SshAgentFacts {
    SshAgentFacts::Unreachable {
        endpoint: endpoint.into(),
        detail: "The system cannot find the file specified. (os error 2)".into(),
        service,
        by_default: endpoint == WIN_PIPE,
    }
}

#[test]
fn ssh_agent_with_keys_is_ok_and_names_only_the_families() {
    let r = ssh_check("ssh_agent_keys", &windows(), None);
    let c = r.check(CheckId::SshAgent).unwrap();
    assert_eq!(c.status, Status::Ok);
    assert!(
        c.summary.contains("2 keys (ed25519: 1, rsa: 1)"),
        "{}",
        c.summary
    );
}

#[test]
fn ssh_agent_counts_a_single_key_and_repeated_families() {
    let keys = |families: &[&str]| {
        let fake = linux_with(SshAgentFacts::Keys {
            endpoint: SOCK.into(),
            families: families.iter().map(|f| (*f).to_owned()).collect(),
        });
        run(&fake, &Options::default())
            .check(CheckId::SshAgent)
            .unwrap()
            .summary
            .clone()
    };
    assert!(keys(&["rsa"]).contains("holds 1 key (rsa: 1)"));
    assert!(keys(&["rsa", "ecdsa", "rsa", "rsa"]).contains("4 keys (ecdsa: 1, rsa: 3)"));
}

#[test]
fn ssh_agent_missing_on_windows_names_the_pipe_and_both_fixes() {
    let r = ssh_check(
        "ssh_agent_missing_windows",
        &windows_with(unreachable(WIN_PIPE, None)),
        Some(Finding::SshAgentMissing),
    );
    assert_eq!(r.check(CheckId::SshAgent).unwrap().status, Status::Info);
}

#[test]
fn ssh_agent_service_disabled_stopped_or_missing_says_what_to_do() {
    for (name, service) in [
        ("ssh_agent_service_disabled", AgentService::Disabled),
        ("ssh_agent_service_stopped", AgentService::Stopped),
        ("ssh_agent_service_missing", AgentService::Missing),
    ] {
        let fake = windows_with(unreachable(WIN_PIPE, Some(service)));
        ssh_check(name, &fake, Some(Finding::SshAgentMissing));
    }
}

#[test]
fn ssh_agent_missing_on_linux_says_to_set_the_socket() {
    ssh_check(
        "ssh_agent_missing_linux",
        &linux_with(unreachable(SOCK, None)),
        Some(Finding::SshAgentMissing),
    );
    ssh_check(
        "ssh_agent_no_socket_variable",
        &linux_with(SshAgentFacts::NoEndpoint),
        Some(Finding::SshAgentMissing),
    );
}

/// An absent agent nobody asked for is a normal state (the CI VMs have none), so it is info and
/// the report stays all ok or info; an agent the user named that does not answer is a warning.
#[test]
fn ssh_agent_absent_by_default_is_info_and_a_named_one_that_fails_is_a_warning() {
    let status_of = |fake: &Fake| {
        run(fake, &Options::default())
            .check(CheckId::SshAgent)
            .unwrap()
            .status
    };
    assert_eq!(
        status_of(&linux_with(SshAgentFacts::NoEndpoint)),
        Status::Info
    );
    for service in [None, Some(AgentService::Stopped)] {
        assert_eq!(
            status_of(&windows_with(unreachable(WIN_PIPE, service))),
            Status::Info
        );
    }
    assert_eq!(
        status_of(&linux_with(unreachable(SOCK, None))),
        Status::Warn
    );
    assert_eq!(
        status_of(&windows_with(unreachable(r"\\.\pipe\pageant.alice", None))),
        Status::Warn
    );
}

#[test]
fn ssh_agent_locked_warns() {
    let r = ssh_check(
        "ssh_agent_locked",
        &windows_with(SshAgentFacts::Locked {
            endpoint: WIN_PIPE.into(),
        }),
        Some(Finding::SshAgentLocked),
    );
    assert_eq!(r.check(CheckId::SshAgent).unwrap().status, Status::Warn);
}

#[test]
fn ssh_agent_without_keys_is_info() {
    let r = ssh_check(
        "ssh_agent_empty",
        &windows_with(SshAgentFacts::Keys {
            endpoint: WIN_PIPE.into(),
            families: vec![],
        }),
        Some(Finding::SshAgentEmpty),
    );
    assert_eq!(r.check(CheckId::SshAgent).unwrap().status, Status::Info);
}

#[test]
fn ssh_agent_that_waits_for_the_user_says_so_instead_of_waiting() {
    let r = ssh_check(
        "ssh_agent_slow",
        &windows_with(SshAgentFacts::TimedOut {
            endpoint: WIN_PIPE.into(),
            limit: Duration::from_secs(2),
        }),
        Some(Finding::SshAgentSlow),
    );
    let c = r.check(CheckId::SshAgent).unwrap();
    assert_eq!(c.status, Status::Info);
    assert!(
        c.summary
            .contains("did not answer within 2 s; it may be waiting for you to unlock it")
    );
}

#[test]
fn ssh_agent_that_is_not_one_is_info_with_the_detail() {
    let r = ssh_check(
        "ssh_agent_unusable",
        &linux_with(SshAgentFacts::Unusable {
            endpoint: SOCK.into(),
            detail: "the agent's answer is not valid: not an identities answer".into(),
        }),
        Some(Finding::SshAgentUnusable),
    );
    assert_eq!(r.check(CheckId::SshAgent).unwrap().status, Status::Info);
}

#[test]
fn no_ssh_agent_result_is_ever_a_failure() {
    for ssh in [
        SshAgentFacts::NoEndpoint,
        unreachable(WIN_PIPE, Some(AgentService::Disabled)),
        SshAgentFacts::Locked {
            endpoint: WIN_PIPE.into(),
        },
        SshAgentFacts::Keys {
            endpoint: WIN_PIPE.into(),
            families: vec![],
        },
    ] {
        let r = run(&windows_with(ssh), &Options::default());
        assert_ne!(r.check(CheckId::SshAgent).unwrap().status, Status::Fail);
        assert!(r.ok);
    }
}

#[test]
fn encryption_is_ok_on_an_encrypted_machine_and_with_no_swap() {
    let r = check("encryption_ok", &linux(), CheckId::DiskEncryption, None);
    assert_eq!(r.check(CheckId::DiskEncryption).unwrap().status, Status::Ok);
    let fake = Fake {
        enc: DiskEncryptionFacts {
            data: DataEncryption::Volume {
                dir: PathBuf::from(DATA),
                protection: Protection::Encrypted { by: "LUKS2".into() },
            },
            swap: SwapFacts::None,
        },
        ..linux()
    };
    check("encryption_no_swap", &fake, CheckId::DiskEncryption, None);
}

#[test]
fn a_missing_data_folder_is_unknown_not_ok_even_with_no_swap() {
    let fake = Fake {
        enc: DiskEncryptionFacts {
            data: DataEncryption::NoFolder,
            swap: SwapFacts::None,
        },
        ..linux()
    };
    let r = check(
        "encryption_no_folder",
        &fake,
        CheckId::DiskEncryption,
        Some(Finding::DiskEncryptionUnknown),
    );
    let c = r.check(CheckId::DiskEncryption).unwrap();
    assert_eq!(c.status, Status::Info);
    assert!(c.detail.as_deref().unwrap().contains("data folder"));
}

#[test]
fn an_unencrypted_drive_warns_and_names_what_it_means_for_secrets() {
    let fake = Fake {
        enc: encryption(
            Protection::NotEncrypted,
            Protection::Encrypted { by: "LUKS2".into() },
        ),
        ..linux()
    };
    let r = check(
        "encryption_drive_not_encrypted",
        &fake,
        CheckId::DiskEncryption,
        Some(Finding::DiskNotEncrypted),
    );
    let c = r.check(CheckId::DiskEncryption).unwrap();
    assert_eq!(c.status, Status::Warn);
    assert!(c.fix.as_deref().unwrap().contains("secrets"));
    assert!(r.ok, "a warning never fails the report");
}

#[test]
fn an_unencrypted_swap_warns_on_every_os() {
    for (name, fake) in [
        ("encryption_swap_not_encrypted_linux", linux()),
        ("encryption_swap_not_encrypted_windows", windows()),
        (
            "encryption_swap_not_encrypted_macos",
            Fake {
                os: Os::MacOs,
                ..linux()
            },
        ),
    ] {
        let fake = Fake {
            enc: encryption(
                Protection::Encrypted { by: "x".into() },
                Protection::NotEncrypted,
            ),
            ..fake
        };
        check(
            name,
            &fake,
            CheckId::DiskEncryption,
            Some(Finding::SwapNotEncrypted),
        );
    }
}

#[test]
fn an_unreadable_state_is_info_and_swap_in_memory_is_fine() {
    let fake = Fake {
        enc: DiskEncryptionFacts {
            data: DataEncryption::Volume {
                dir: PathBuf::from(DATA),
                protection: Protection::Unknown {
                    why: "PowerShell did not answer within 8s".into(),
                },
            },
            swap: SwapFacts::Areas(vec![SwapArea {
                name: "/dev/zram0".into(),
                protection: Protection::NotOnDisk {
                    what: "compressed memory (zram)".into(),
                },
            }]),
        },
        ..windows()
    };
    let r = check(
        "encryption_unknown",
        &fake,
        CheckId::DiskEncryption,
        Some(Finding::DiskEncryptionUnknown),
    );
    assert_eq!(
        r.check(CheckId::DiskEncryption).unwrap().status,
        Status::Info
    );
}

#[test]
fn an_unreadable_swap_list_with_an_unencrypted_drive_still_warns_and_keeps_the_reason() {
    let fake = Fake {
        enc: DiskEncryptionFacts {
            data: DataEncryption::Volume {
                dir: PathBuf::from(DATA),
                protection: Protection::NotEncrypted,
            },
            swap: SwapFacts::Unreadable {
                why: "/proc/swaps: permission denied".into(),
            },
        },
        ..linux()
    };
    let r = check(
        "encryption_swap_list_unreadable",
        &fake,
        CheckId::DiskEncryption,
        Some(Finding::DiskNotEncrypted),
    );
    let c = r.check(CheckId::DiskEncryption).unwrap();
    assert!(c.detail.as_deref().unwrap().contains("/proc/swaps"));
    assert_eq!(SwapFacts::None.areas().len(), 0);
}

#[test]
fn drive_and_swap_both_unencrypted_on_an_unknown_os_gets_generic_advice() {
    let fake = Fake {
        os: Os::Other,
        enc: encryption(Protection::NotEncrypted, Protection::NotEncrypted),
        ..linux()
    };
    let r = check(
        "encryption_both_not_encrypted_other_os",
        &fake,
        CheckId::DiskEncryption,
        Some(Finding::DiskNotEncrypted),
    );
    let fix = r
        .check(CheckId::DiskEncryption)
        .unwrap()
        .fix
        .clone()
        .unwrap();
    assert!(
        fix.starts_with("The drive and the swap are not encrypted"),
        "{fix}"
    );
}

#[test]
fn a_missing_azure_cli_is_a_warning_and_the_report_stays_ok() {
    let fake = Fake {
        az: AzCliFacts::NotFound,
        ..windows()
    };
    let r = check(
        "az_cli_missing",
        &fake,
        CheckId::AzCli,
        Some(Finding::AzCliMissing),
    );
    assert!(r.ok, "the Azure CLI never fails the report");
    assert_eq!(r.check(CheckId::AzCli).unwrap().status, Status::Warn);
}

#[test]
fn a_credential_store_that_takes_a_write_is_ok() {
    let r = run(&linux(), &Options::default());
    let check = r.check(CheckId::CredentialStore).unwrap();
    assert_eq!(check.status, Status::Ok);
    assert_eq!(check.finding, None);
}

#[test]
fn a_linux_host_with_no_credential_store_gets_the_headless_fix_as_a_warning() {
    let fake = Fake {
        store: StoreHealth::NoProvider,
        ..linux()
    };
    let r = check(
        "credential_store_missing_linux",
        &fake,
        CheckId::CredentialStore,
        Some(Finding::CredentialStoreMissing),
    );
    assert!(r.ok, "a store that refuses writes does not stop sandboxes");
    let fix = r
        .check(CheckId::CredentialStore)
        .unwrap()
        .fix
        .clone()
        .unwrap();
    assert!(fix.contains("apt install gnome-keyring"), "{fix}");
    assert!(fix.contains("restart Puddle"), "{fix}");
}

#[test]
fn a_locked_credential_store_names_the_unlock_command() {
    let fake = Fake {
        store: StoreHealth::Locked,
        ..linux()
    };
    let r = check(
        "credential_store_locked_linux",
        &fake,
        CheckId::CredentialStore,
        Some(Finding::CredentialStoreLocked),
    );
    let fix = r
        .check(CheckId::CredentialStore)
        .unwrap()
        .fix
        .clone()
        .unwrap();
    assert!(
        fix.contains("gnome-keyring-daemon --daemonize --login"),
        "{fix}"
    );
}

#[test]
fn the_windows_credential_store_advice_does_not_name_gnome_keyring() {
    let fake = Fake {
        store: StoreHealth::Locked,
        ..windows()
    };
    let r = run(&fake, &Options::default());
    let fix = r
        .check(CheckId::CredentialStore)
        .unwrap()
        .fix
        .clone()
        .unwrap();
    assert!(!fix.contains("gnome-keyring"), "{fix}");
}
