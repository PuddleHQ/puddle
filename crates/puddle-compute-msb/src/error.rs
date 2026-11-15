// SPDX-License-Identifier: AGPL-3.0-or-later
//! SDK errors to [`ComputeError`].

use microsandbox::MicrosandboxError;
use puddle_compute::{ComputeError, StartRefusal};
use puddle_types::WorkspaceStatus;

/// How msb begins the error of a start it refuses because it cannot prove the sandbox's stored
/// disk state came from the host (an upgrade found an unrecorded or changed journal).
const NEEDS_RECOVERY: &str = "migration/recovery required";

/// Maps an SDK error from operation `op` on `target` (a sandbox name, or the image or volume the
/// call was about) to a [`ComputeError`]. Matches on the SDK's variants; anything without a
/// counterpart becomes [`ComputeError::Runtime`] with the SDK's message.
#[must_use]
pub fn map(op: &'static str, target: &str, error: MicrosandboxError) -> ComputeError {
    match error {
        MicrosandboxError::SandboxNotFound(_) => ComputeError::NotFound {
            sandbox: target.to_owned(),
        },
        MicrosandboxError::SandboxAlreadyExists(_) => ComputeError::AlreadyExists {
            sandbox: target.to_owned(),
        },
        MicrosandboxError::SandboxReplaced { .. } => ComputeError::StaleHandle {
            sandbox: target.to_owned(),
        },
        MicrosandboxError::SandboxStillRunning(_) => ComputeError::InvalidState {
            sandbox: target.to_owned(),
            op,
            status: WorkspaceStatus::Running,
        },
        MicrosandboxError::SandboxNotRunning(_) => ComputeError::InvalidState {
            sandbox: target.to_owned(),
            op,
            status: WorkspaceStatus::Stopped,
        },
        MicrosandboxError::VolumeNotFound(volume) => ComputeError::VolumeNotFound { volume },
        MicrosandboxError::VolumeAlreadyExists(volume) => ComputeError::VolumeExists { volume },
        e @ (MicrosandboxError::ImageNotFound(_) | MicrosandboxError::Image(_)) => {
            ComputeError::ImagePull {
                image: target.to_owned(),
                reason: e.to_string(),
            }
        }
        MicrosandboxError::InvalidConfig(reason) => ComputeError::InvalidSpec { reason },
        MicrosandboxError::Runtime(message) if message.starts_with(NEEDS_RECOVERY) => {
            tracing::warn!(sandbox = target, op, %message, "msb kept the sandbox for recovery");
            ComputeError::StartRefused {
                sandbox: target.to_owned(),
                cause: StartRefusal::RootDiskNeedsRecovery,
            }
        }
        other => runtime(op, &other),
    }
}

/// [`ComputeError::Runtime`] with `error`'s message.
#[must_use]
pub(crate) fn runtime(op: &'static str, error: &dyn std::fmt::Display) -> ComputeError {
    ComputeError::Runtime {
        op,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
