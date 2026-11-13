// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use puddle_doctor::{
    BootFacts, CodeIntegrityFacts, DiskFacts, GsaFacts, HypervisorApi, HypervisorFacts, JobFacts,
    KeptLogs, Os, ProcessOutcome, RuntimeFacts, RuntimeState,
};

use super::*;
use crate::cli;

struct Fake {
    runtime_ready: bool,
    /// Makes the test boot fail, with these logs kept.
    failed_boot_kept: Option<KeptLogs>,
    /// Bytes free on the data folder's drive.
    free_bytes: u64,
}

impl Probe for Fake {
    fn os(&self) -> Os {
        Os::Linux
    }
    fn arch(&self) -> String {
        "x86_64".into()
    }
    fn hypervisor(&self) -> HypervisorFacts {
        HypervisorFacts {
            api: HypervisorApi::Ready,
            firmware_virtualization: Some(true),
            hypervisor_vendor: None,
        }
    }
    fn code_integrity(&self) -> Option<CodeIntegrityFacts> {
        None
    }
    fn data_path(&self) -> Option<puddle_doctor::DataPathFacts> {
        None
    }
    fn runtime(&self) -> RuntimeFacts {
        RuntimeFacts {
            dir: PathBuf::from("/opt/puddle/runtime"),
            msb: PathBuf::from("/opt/puddle/runtime/msb"),
            expected: "0.7.7-puddle.2".into(),
            state: if self.runtime_ready {
                RuntimeState::Ready {
                    version: "0.7.7-puddle.2".into(),
                    overridden: false,
                }
            } else {
                RuntimeState::Unusable(puddle_runtime_missing())
            },
        }
    }
    fn launch(&self, _limit: Duration) -> ProcessOutcome {
        ProcessOutcome::Exited {
            code: Some(0),
            stdout_first_line: "msb 0.7.7-puddle.2".into(),
            stderr_tail: String::new(),
            elapsed: Duration::from_millis(50),
        }
    }
    fn boot(&self, _limit: Duration) -> BootFacts {
        if let Some(kept) = &self.failed_boot_kept {
            return BootFacts::Ran {
                outcome: ProcessOutcome::Exited {
                    code: Some(1),
                    stdout_first_line: String::new(),
                    stderr_tail: String::new(),
                    elapsed: Duration::from_millis(900),
                },
                kept_logs: Some(kept.clone()),
            };
        }
        BootFacts::Ran {
            outcome: ProcessOutcome::Exited {
                code: Some(puddle_doctor::PROBE_EXIT_CODE),
                stdout_first_line: String::new(),
                stderr_tail: String::new(),
                elapsed: Duration::from_millis(900),
            },
            kept_logs: None,
        }
    }
    fn job(&self) -> Option<JobFacts> {
        None
    }
    fn global_secure_access(&self) -> Option<GsaFacts> {
        None
    }
    fn disk_space(&self) -> DiskFacts {
        DiskFacts::Read {
            dir: PathBuf::from("/data/puddle"),
            free_bytes: self.free_bytes,
        }
    }
    fn disk_encryption(&self) -> puddle_doctor::DiskEncryptionFacts {
        puddle_doctor::DiskEncryptionFacts {
            data: puddle_doctor::DataEncryption::Volume {
                dir: PathBuf::from("/data/puddle"),
                protection: puddle_doctor::Protection::Encrypted { by: "LUKS2".into() },
            },
            swap: puddle_doctor::SwapFacts::None,
        }
    }
    fn ssh_agent(&self, _limit: Duration) -> puddle_doctor::SshAgentFacts {
        puddle_doctor::SshAgentFacts::Keys {
            endpoint: "/run/agent.sock".into(),
            families: vec!["ed25519".into()],
        }
    }
    fn az_cli(&self, _limit: Duration) -> puddle_doctor::AzCliFacts {
        puddle_doctor::AzCliFacts::Found {
            version: "2.64.0".into(),
        }
    }
}

const GB: u64 = 1024 * 1024 * 1024;

/// What the checks use when the settings say nothing.
const OWN_LINES: (u64, u64) = (20 * GB, 5 * GB);

fn puddle_runtime_missing() -> puddle_doctor::RuntimeError {
    puddle_doctor::RuntimeError::Missing {
        path: PathBuf::from("/opt/puddle/runtime/msb"),
    }
}

#[test]
fn flags() {
    assert_eq!(
        cli::parse(["doctor"]),
        Ok(Command::Doctor(DoctorArgs::default()))
    );
    assert_eq!(
        cli::parse(["doctor", "--no-boot", "--json"]),
        Ok(Command::Doctor(DoctorArgs {
            json: true,
            no_boot: true,
            flags: Flags::new(),
        }))
    );
    assert_eq!(
        cli::parse(["doctor", "--json", "--json"]),
        Err(UsageError::UnknownArgument("--json".into()))
    );
    assert_eq!(
        cli::parse(["doctor", "--fix"]),
        Err(UsageError::UnknownArgument("--fix".into()))
    );
}

#[test]
fn the_data_folder_options_are_taken_like_serves() {
    let Ok(Command::Doctor(args)) = cli::parse(["doctor", "--data-dir", "d", "--config", "c.toml"])
    else {
        panic!("doctor takes both options");
    };
    assert_eq!(
        args.flags.get("data_dir"),
        Some(&crate::cmd::serve::options::Value::Path("d".into()))
    );
    assert_eq!(
        args.flags.get("config"),
        Some(&crate::cmd::serve::options::Value::Path("c.toml".into()))
    );
    assert_eq!(
        cli::parse(["doctor", "--data-dir"]),
        Err(UsageError::MissingArgument("--data-dir <path>"))
    );
    assert_eq!(
        cli::parse(["doctor", "--data-dir", "a", "--data-dir", "b"]),
        Err(UsageError::UnknownArgument("--data-dir".into()))
    );
}

#[test]
fn healthy_text_report_exits_zero() {
    let (text, code) = report(
        &Fake {
            runtime_ready: true,
            failed_boot_kept: None,
            free_bytes: 100 * GB,
        },
        &DoctorArgs::default(),
        OWN_LINES,
    );
    assert!(text.starts_with("puddle doctor (puddle "), "{text}");
    assert!(text.contains("No problems found."), "{text}");
    assert_eq!(code, ExitCode::SUCCESS);
}

#[test]
fn json_report_with_a_failure_exits_one() {
    let args = DoctorArgs {
        json: true,
        no_boot: true,
        ..DoctorArgs::default()
    };
    let (json, code) = report(
        &Fake {
            runtime_ready: false,
            failed_boot_kept: None,
            free_bytes: 100 * GB,
        },
        &args,
        OWN_LINES,
    );
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["schema_version"], puddle_doctor::SCHEMA_VERSION);
    assert_eq!(v["ok"], false);
    let runtime = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "runtime")
        .unwrap();
    assert_eq!(runtime["finding"], "runtime_missing");
    assert!(json.ends_with("}\n"));
    assert_eq!(code, ExitCode::FAILURE);
}

#[test]
fn no_boot_skips_the_test_boot() {
    let args = DoctorArgs {
        no_boot: true,
        ..DoctorArgs::default()
    };
    let (text, _) = report(
        &Fake {
            runtime_ready: true,
            failed_boot_kept: None,
            free_bytes: 100 * GB,
        },
        &args,
        OWN_LINES,
    );
    assert!(text.contains("not checked: --no-boot"), "{text}");
}

fn failed_boot_report(json: bool) -> String {
    let probe = Fake {
        runtime_ready: true,
        failed_boot_kept: Some(KeptLogs::Kept {
            dir: PathBuf::from("/data/puddle/doctor-boot-logs/puddle-doctor-boot-1"),
            failed: vec![],
        }),
        free_bytes: 100 * GB,
    };
    let args = DoctorArgs {
        json,
        ..DoctorArgs::default()
    };
    report(&probe, &args, OWN_LINES).0
}

#[test]
fn a_failed_boot_names_where_its_logs_are_kept_in_the_text_report() {
    let text = failed_boot_report(false);
    // The text report wraps long lines, so the folder is on its own line.
    assert!(text.contains("logs of this boot are kept in"), "{text}");
    assert!(
        text.contains("/data/puddle/doctor-boot-logs/puddle-doctor-boot-1"),
        "{text}"
    );
}

#[test]
fn a_failed_boot_names_where_its_logs_are_kept_in_the_json_report() {
    let json = failed_boot_report(true);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let boot = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "test_boot")
        .unwrap();
    assert!(
        boot["detail"]
            .as_str()
            .unwrap()
            .contains("kept in /data/puddle/doctor-boot-logs/"),
        "{boot}"
    );
}

fn installed_probe() -> SystemProbe {
    SystemProbe::installed(Path::new("/opt/puddle/puddle")).unwrap()
}

#[test]
fn the_command_keeps_failed_boot_logs_in_the_data_folder() {
    let probe = keeping_logs(installed_probe(), Some(Path::new("/data/puddle")));
    assert_eq!(
        probe.keep_boot_logs_dir(),
        Some(Path::new("/data/puddle/doctor-boot-logs"))
    );
}

#[test]
fn a_missing_data_folder_is_none_and_a_found_one_is_used() {
    assert_eq!(data_folder(Err(puddle_fs::DataDirError::NoBase)), None);
    assert_eq!(
        data_folder(Ok(PathBuf::from("/data/puddle"))),
        Some(PathBuf::from("/data/puddle"))
    );
}

#[test]
fn without_a_data_folder_nothing_is_kept() {
    assert_eq!(
        keeping_logs(installed_probe(), None).keep_boot_logs_dir(),
        None
    );
}
fn disk_report(free_bytes: u64, lines: (u64, u64)) -> (String, ExitCode) {
    let probe = Fake {
        runtime_ready: true,
        failed_boot_kept: None,
        free_bytes,
    };
    let args = DoctorArgs {
        no_boot: true,
        ..DoctorArgs::default()
    };
    report(&probe, &args, lines)
}

#[test]
fn little_free_space_warns_and_exits_zero() {
    let (text, code) = disk_report(12 * GB, OWN_LINES);
    assert!(text.contains("warn  Disk space"), "{text}");
    assert!(
        text.contains("12 GB free on the drive that holds Puddle's data"),
        "{text}"
    );
    assert_eq!(code, ExitCode::SUCCESS);
}

#[test]
fn very_little_free_space_fails_and_exits_one() {
    let (text, code) = disk_report(3 * GB, OWN_LINES);
    assert!(text.contains("FAIL  Disk space"), "{text}");
    assert_eq!(code, ExitCode::FAILURE);
}

#[test]
fn the_lines_passed_in_decide() {
    let (text, code) = disk_report(12 * GB, (10 * GB, 2 * GB));
    assert!(text.contains("ok    Disk space"), "{text}");
    assert_eq!(code, ExitCode::SUCCESS);
    let (_, code) = disk_report(12 * GB, (50 * GB, 40 * GB));
    assert_eq!(code, ExitCode::FAILURE);
}

#[test]
fn the_lines_come_from_the_settings_in_the_data_folder_or_are_puddles_own() {
    let data = tempfile::tempdir().unwrap();
    assert_eq!(free_space_lines(None), OWN_LINES);
    assert_eq!(
        free_space_lines(Some(data.path())),
        OWN_LINES,
        "no settings file"
    );
    let settings = data.path().join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("global.json"),
        r#"{"schema_version": 2, "ui": {"disk_warn_free_gb": 30, "disk_fail_free_gb": 7}}"#,
    )
    .unwrap();
    assert_eq!(free_space_lines(Some(data.path())), (30 * GB, 7 * GB));
    std::fs::write(settings.join("global.json"), "{not json").unwrap();
    assert_eq!(
        free_space_lines(Some(data.path())),
        OWN_LINES,
        "damaged file"
    );
}
