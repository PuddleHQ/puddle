// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn sdk_variants_map_to_their_counterparts() {
    let m = |e| map("start", "box", e);
    assert_eq!(
        m(MicrosandboxError::SandboxNotFound("box".into())),
        ComputeError::NotFound {
            sandbox: "box".into()
        }
    );
    assert_eq!(
        m(MicrosandboxError::SandboxAlreadyExists("box".into())),
        ComputeError::AlreadyExists {
            sandbox: "box".into()
        }
    );
    assert_eq!(
        m(MicrosandboxError::SandboxReplaced {
            name: "box".into(),
            expected: "1".into(),
            actual: "2".into(),
        }),
        ComputeError::StaleHandle {
            sandbox: "box".into()
        }
    );
    assert_eq!(
        m(MicrosandboxError::SandboxStillRunning("box".into())),
        ComputeError::InvalidState {
            sandbox: "box".into(),
            op: "start",
            status: WorkspaceStatus::Running
        }
    );
    assert_eq!(
        m(MicrosandboxError::SandboxNotRunning(
            "box is stopped".into()
        )),
        ComputeError::InvalidState {
            sandbox: "box".into(),
            op: "start",
            status: WorkspaceStatus::Stopped
        }
    );
    assert_eq!(
        m(MicrosandboxError::VolumeNotFound("ws-a".into())),
        ComputeError::VolumeNotFound {
            volume: "ws-a".into()
        }
    );
    assert_eq!(
        m(MicrosandboxError::VolumeAlreadyExists("ws-a".into())),
        ComputeError::VolumeExists {
            volume: "ws-a".into()
        }
    );
    assert_eq!(
        m(MicrosandboxError::InvalidConfig("bad".into())),
        ComputeError::InvalidSpec {
            reason: "bad".into()
        }
    );
}

#[test]
fn image_errors_name_the_image() {
    let e = map(
        "pull image",
        "x.invalid/none:0",
        MicrosandboxError::ImageNotFound("x.invalid/none:0".into()),
    );
    assert!(
        matches!(&e, ComputeError::ImagePull { image, reason }
                if image == "x.invalid/none:0" && reason.contains("image not found")),
        "{e:?}"
    );
}

#[test]
fn a_disk_msb_cannot_vouch_for_refuses_the_start_without_its_words() {
    // msb's wording at v0.7.12 (`checkpoint/layout.rs`, `recovery`).
    let e = map(
        "start",
        "box",
        MicrosandboxError::Runtime(
            "migration/recovery required: unrecorded protected storage at root-disk.json; \
                 preserve this sandbox and use a verified backup or recover its disk chain \
                 before retrying"
                .into(),
        ),
    );
    assert_eq!(
        e,
        ComputeError::StartRefused {
            sandbox: "box".into(),
            cause: StartRefusal::RootDiskNeedsRecovery
        }
    );
}

#[test]
fn everything_else_keeps_the_sdk_message() {
    assert_eq!(
        map("stop", "box", MicrosandboxError::Runtime("vmm gone".into())),
        ComputeError::Runtime {
            op: "stop",
            message: "runtime error: vmm gone".into()
        }
    );
    assert_eq!(
        map("exec", "box", MicrosandboxError::Custom("boom".into())),
        ComputeError::Runtime {
            op: "exec",
            message: "boom".into()
        }
    );
}
