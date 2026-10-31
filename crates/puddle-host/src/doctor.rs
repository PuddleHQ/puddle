// SPDX-License-Identifier: GPL-3.0-or-later
//! The system check the API serves: `puddle doctor`'s checks run against this machine and the
//! runtime folder the host was started with.

use std::path::Path;
use std::sync::Arc;

use puddle_api::HostDoctor;
use puddle_doctor::boot::boot_logs_dir;
use puddle_doctor::{Options, SystemProbe, diagnose};
use puddle_runtime::{DevOverride, RuntimeLayout, RuntimeVersion};

use crate::HostPaths;

/// A service that runs every check, with or without the test boot, on the real machine. Each run
/// opens the runtime afresh, so a fix made while Puddle runs (a permission, a replaced file) shows
/// at the next run. A failed test boot keeps msb's logs in the data folder, where `puddle doctor`
/// keeps its own.
pub(crate) fn system_doctor(
    layout: &RuntimeLayout,
    paths: &HostPaths,
    expected: &RuntimeVersion,
) -> Arc<HostDoctor> {
    let dir = layout.runtime_dir().to_path_buf();
    let data = paths.data().to_path_buf();
    let expected = *expected;
    Arc::new(HostDoctor::new(Arc::new(move |boot| {
        let probe = probe(&dir, expected, &data);
        let options = Options {
            boot,
            ..Options::default()
        };
        diagnose(&probe, &options, puddle_types::VERSION)
    })))
}

/// The probe of the runtime in `runtime_dir`, keeping failed boots' logs under `data`.
fn probe(runtime_dir: &Path, expected: RuntimeVersion, data: &Path) -> SystemProbe {
    SystemProbe::new(runtime_dir.to_path_buf(), expected, DevOverride::from_env())
        .with_keep_boot_logs(boot_logs_dir(data))
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
}
