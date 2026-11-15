// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn messages_name_the_thing_and_the_cause() {
    let cases = [
        (
            ComputeError::NotFound {
                sandbox: "a".into(),
            },
            r#"sandbox "a" not found"#,
        ),
        (
            ComputeError::AlreadyExists {
                sandbox: "a".into(),
            },
            r#"sandbox "a" already exists"#,
        ),
        (
            ComputeError::StaleDir {
                sandbox: "a".into(),
            },
            "stale directory",
        ),
        (
            ComputeError::InvalidState {
                sandbox: "a".into(),
                op: "exec",
                status: WorkspaceStatus::Stopped,
            },
            r#"cannot exec sandbox "a" while it is stopped"#,
        ),
        (
            ComputeError::StaleHandle {
                sandbox: "a".into(),
            },
            "earlier boot",
        ),
        (
            ComputeError::InvalidSpec { reason: "x".into() },
            "invalid sandbox spec: x",
        ),
        (
            ComputeError::VolumeNotFound { volume: "v".into() },
            r#"volume "v" not found"#,
        ),
        (
            ComputeError::VolumeExists { volume: "v".into() },
            r#"volume "v" already exists"#,
        ),
        (
            ComputeError::VolumeInUse {
                volume: "v".into(),
                holder: "a".into(),
            },
            r#"volume "v" is in use by sandbox "a""#,
        ),
        (
            ComputeError::VolumeSizeMismatch {
                volume: "v".into(),
                existing_mib: 2048,
                requested_mib: 1024,
            },
            "has 2048 MiB but 1024 MiB",
        ),
        (
            ComputeError::ImagePull {
                image: "i".into(),
                reason: "not found".into(),
            },
            r#"image "i" could not be pulled: not found"#,
        ),
        (
            ComputeError::ExecTimeout {
                sandbox: "a".into(),
                program: "sleep".into(),
                timeout: Duration::from_secs(1),
            },
            "timed out after 1s",
        ),
        (
            ComputeError::StartRefused {
                sandbox: "a".into(),
                cause: StartRefusal::RootDiskNeedsRecovery,
            },
            r#"start of sandbox "a" refused, nothing was attached: the runtime cannot verify its stored root disk after an upgrade"#,
        ),
        (
            ComputeError::Runtime {
                op: "start",
                message: "boom".into(),
            },
            "runtime failed to start: boom",
        ),
    ];
    for (err, want) in cases {
        assert!(err.to_string().contains(want), "{err} lacks {want}");
    }
}
