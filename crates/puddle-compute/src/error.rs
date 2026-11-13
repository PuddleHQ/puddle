// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute plane's error type.

use std::time::Duration;

use puddle_types::WorkspaceStatus;

/// Why a compute-plane call failed. Names are plain strings because a runtime may report names
/// Puddle didn't create (a foreign sandbox, a stale directory).
///
/// The enum is `Clone` + `Eq` so the fake can inject the same error repeatedly and tests can
/// compare errors directly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ComputeError {
    /// No sandbox has this name.
    #[error("sandbox {sandbox:?} not found")]
    NotFound {
        /// The name asked for.
        sandbox: String,
    },
    /// A sandbox with this name exists already.
    #[error("sandbox {sandbox:?} already exists")]
    AlreadyExists {
        /// The name.
        sandbox: String,
    },
    /// The name is blocked by a directory a failed create left behind (an upstream msb
    /// bug). Remove it with [`crate::Runtime::remove_stale_dir`].
    #[error("sandbox name {sandbox:?} is blocked by a stale directory from a failed create")]
    StaleDir {
        /// The blocked name.
        sandbox: String,
    },
    /// The sandbox is in a state that doesn't allow the operation.
    #[error("cannot {op} sandbox {sandbox:?} while it is {status}")]
    InvalidState {
        /// The sandbox.
        sandbox: String,
        /// What was attempted (`"exec"`, `"remove"`, ...).
        op: &'static str,
        /// The state it was in.
        status: WorkspaceStatus,
    },
    /// The handle refers to an earlier boot of the sandbox, which has since been restarted.
    #[error("handle for sandbox {sandbox:?} refers to an earlier boot")]
    StaleHandle {
        /// The sandbox.
        sandbox: String,
    },
    /// The spec is inconsistent (see [`crate::SandboxSpec::validate`]).
    #[error("invalid sandbox spec: {reason}")]
    InvalidSpec {
        /// What is wrong.
        reason: String,
    },
    /// No volume has this name.
    #[error("volume {volume:?} not found")]
    VolumeNotFound {
        /// The name asked for.
        volume: String,
    },
    /// A volume with this name exists already.
    #[error("volume {volume:?} already exists")]
    VolumeExists {
        /// The name.
        volume: String,
    },
    /// The volume is attached to a running sandbox (single writer, ADR 0006 point 8).
    #[error("volume {volume:?} is in use by sandbox {holder:?}")]
    VolumeInUse {
        /// The volume.
        volume: String,
        /// The sandbox that holds it.
        holder: String,
    },
    /// The existing volume's size differs from the size asked for.
    #[error("volume {volume:?} has {existing_mib} MiB but {requested_mib} MiB was requested")]
    VolumeSizeMismatch {
        /// The volume.
        volume: String,
        /// Its size.
        existing_mib: u32,
        /// The size in the spec.
        requested_mib: u32,
    },
    /// The image couldn't be pulled or inspected.
    #[error("image {image:?} could not be pulled: {reason}")]
    ImagePull {
        /// The reference.
        image: String,
        /// Why.
        reason: String,
    },
    /// A command ran longer than its [`crate::ExecRequest::timeout`].
    #[error("command {program:?} in sandbox {sandbox:?} timed out after {timeout:?}")]
    ExecTimeout {
        /// The sandbox.
        sandbox: String,
        /// The program.
        program: String,
        /// The timeout that expired.
        timeout: Duration,
    },
    /// The runtime refused to start the sandbox because of what its own folder holds, and
    /// attached nothing. The cause is a closed set: it carries no text the guest wrote.
    #[error("start of sandbox {sandbox:?} refused, nothing was attached: {cause}")]
    StartRefused {
        /// The sandbox.
        sandbox: String,
        /// Why.
        cause: StartRefusal,
    },
    /// Anything else the runtime reported.
    #[error("runtime failed to {op}: {message}")]
    Runtime {
        /// What was attempted.
        op: &'static str,
        /// The runtime's message.
        message: String,
    },
}

/// Why a runtime refused a start ([`ComputeError::StartRefused`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StartRefusal {
    /// The runtime cannot vouch for the sandbox's stored root disk: a runtime upgrade found disk
    /// state it cannot prove came from the host (msb's "migration/recovery required"), so it
    /// keeps the sandbox as it is instead of attaching that disk.
    #[error("the runtime cannot verify its stored root disk after an upgrade")]
    RootDiskNeedsRecovery,
}

impl StartRefusal {
    /// What the user can do about it, as a sentence.
    #[must_use]
    pub const fn way_out(self) -> &'static str {
        match self {
            Self::RootDiskNeedsRecovery => {
                "Delete the workspace and create it again to give it a fresh root disk. What \
                 was installed outside the volume is lost; the volume with your work can be kept."
            }
        }
    }
}

#[cfg(test)]
mod tests;
