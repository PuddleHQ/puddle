// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_probe_keeps_failed_boot_logs_in_the_data_folder() {
    let probe = probe(
        Path::new("/opt/puddle/runtime"),
        RuntimeVersion::built_for(),
        Path::new("/data/puddle"),
    );
    assert_eq!(
        probe.keep_boot_logs_dir(),
        Some(Path::new("/data/puddle/doctor-boot-logs"))
    );
}

#[test]
fn the_free_space_lines_default_and_follow_the_settings() {
    use puddle_api::SettingsRepo as _;
    let dir = tempfile::tempdir().unwrap();
    let repo = crate::FileSettings::new(dir.path());
    let own = DiskSpaceLimits::default();
    assert_eq!(disk_limits(&repo), own, "nothing saved");
    assert_eq!(own.warn().bytes(), puddle_doctor::DEFAULT_DISK_WARN_BELOW);
    assert_eq!(own.fail().bytes(), puddle_doctor::DEFAULT_DISK_FAIL_BELOW);

    repo.save_global(serde_json::json!({"ui": {"disk_warn_free_gb": 40, "disk_fail_free_gb": 8}}))
        .unwrap();
    let set = disk_limits(&repo);
    assert_eq!((set.warn().gb(), set.fail().gb()), (40, 8));

    repo.save_global(serde_json::json!({"ui": {"disk_warn_free_gb": 0}}))
        .unwrap();
    assert_eq!(
        disk_limits(&repo),
        own,
        "an invalid value gives the defaults"
    );
    repo.save_global(serde_json::json!("not settings")).unwrap();
    assert_eq!(
        disk_limits(&repo),
        own,
        "damaged settings give the defaults"
    );
}

#[test]
fn the_ssh_agent_pipe_follows_the_settings() {
    use puddle_api::SettingsRepo as _;
    let dir = tempfile::tempdir().unwrap();
    let repo = crate::FileSettings::new(dir.path());
    assert_eq!(ssh_agent_pipe(&repo), None, "nothing saved");
    repo.save_global(serde_json::json!({"ui": {"ssh_agent_pipe": " /run/agent.sock "}}))
        .unwrap();
    assert_eq!(ssh_agent_pipe(&repo).as_deref(), Some("/run/agent.sock"));
    repo.save_global(serde_json::json!("not settings")).unwrap();
    assert_eq!(ssh_agent_pipe(&repo), None, "damaged settings name none");
}

#[test]
fn the_probe_measures_the_data_folders_drive() {
    let dir = tempfile::tempdir().unwrap();
    let probe = probe(
        Path::new("/opt/puddle/runtime"),
        RuntimeVersion::built_for(),
        dir.path(),
    );
    assert!(matches!(
        puddle_doctor::Probe::disk_space(&probe),
        puddle_doctor::DiskFacts::Read { .. }
    ));
}
