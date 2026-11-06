// SPDX-License-Identifier: GPL-3.0-or-later
//! The system check the API serves: `puddle doctor`'s checks run against this machine and the
//! runtime folder the host was started with.

use std::path::Path;
use std::sync::Arc;

use puddle_api::{HostDoctor, SettingsRepo};
use puddle_doctor::boot::boot_logs_dir;
use puddle_doctor::{Options, SystemProbe, diagnose};
use puddle_runtime::{DevOverride, RuntimeLayout, RuntimeVersion};
use puddle_settings::{DiskSpaceLimits, GlobalSettings, UiPrefs};

use crate::HostPaths;

/// A service that runs every check, with or without the test boot, on the real machine. Each run
/// opens the runtime afresh, so a fix made while Puddle runs (a permission, a replaced file) shows
/// at the next run. A failed test boot keeps msb's logs in the data folder, where `puddle doctor`
/// keeps its own. The free-space lines come from the settings at every run, so a change in
/// Settings shows at the next one.
pub(crate) fn system_doctor(
    layout: &RuntimeLayout,
    paths: &HostPaths,
    expected: &RuntimeVersion,
    settings: Arc<dyn SettingsRepo>,
) -> Arc<HostDoctor> {
    let dir = layout.runtime_dir().to_path_buf();
    let data = paths.data().to_path_buf();
    let expected = *expected;
    Arc::new(HostDoctor::new(Arc::new(move |boot| {
        let probe =
            probe(&dir, expected, &data).with_ssh_agent_pipe(ssh_agent_pipe(settings.as_ref()));
        let limits = disk_limits(settings.as_ref());
        let options = Options {
            boot,
            disk_warn_below: limits.warn().bytes(),
            disk_fail_below: limits.fail().bytes(),
            ..Options::default()
        };
        diagnose(&probe, &options, puddle_types::VERSION)
    })))
}

/// The probe of the runtime in `runtime_dir`, keeping failed boots' logs under `data` and
/// measuring the drive that holds `data` and checking the folders above it.
fn probe(runtime_dir: &Path, expected: RuntimeVersion, data: &Path) -> SystemProbe {
    SystemProbe::new(runtime_dir.to_path_buf(), expected, DevOverride::from_env())
        .with_keep_boot_logs(boot_logs_dir(data))
        .with_data_dir(data)
}

/// The window preferences the user's settings hold. Settings that cannot be read or are not valid
/// give Puddle's own (with a warning in the log): a check must not stop because the settings are
/// damaged.
fn ui_prefs(settings: &dyn SettingsRepo) -> UiPrefs {
    let document = match settings.load_global() {
        Ok(Some(document)) => document,
        Ok(None) => return UiPrefs::default(),
        Err(reason) => {
            tracing::warn!(%reason, "using Puddle's own settings for the doctor: the settings cannot be read");
            return UiPrefs::default();
        }
    };
    match GlobalSettings::from_document(document) {
        Ok(loaded) => loaded.settings.ui,
        Err(reason) => {
            tracing::warn!(%reason, "using Puddle's own settings for the doctor: the settings are not valid");
            UiPrefs::default()
        }
    }
}

/// The free-space lines the user's settings set, or Puddle's own when they cannot be used.
#[must_use]
pub fn disk_limits(settings: &dyn SettingsRepo) -> DiskSpaceLimits {
    ui_prefs(settings).disk_limits()
}

/// The pipe or socket of the SSH agent the user's settings name, if they name one.
#[must_use]
pub fn ssh_agent_pipe(settings: &dyn SettingsRepo) -> Option<String> {
    ui_prefs(settings).ssh_agent_pipe().map(str::to_owned)
}

#[cfg(test)]
mod tests {
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

        repo.save_global(
            serde_json::json!({"ui": {"disk_warn_free_gb": 40, "disk_fail_free_gb": 8}}),
        )
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
}
