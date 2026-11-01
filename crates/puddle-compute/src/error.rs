// SPDX-License-Identifier: GPL-3.0-or-later
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
    /// The sandbox's root-disk journal names a file outside the sandbox's own folder. A process
    /// in the sandbox can write that journal, so attaching what it names could hand the sandbox
    /// another workspace's disk or a file of the host.
    #[error("its root-disk journal names a file outside its own folder")]
    RootDiskOutsideFolder,
    /// The root-disk journal cannot be read well enough to tell which files it names.
    #[error(
        "its root-disk journal cannot be read, so the files the start would attach are unknown"
    )]
    RootDiskUnreadable,
}

impl StartRefusal {
    /// What the user can do about it, as a sentence.
    #[must_use]
    pub const fn way_out(self) -> &'static str {
        match self {
            Self::RootDiskOutsideFolder | Self::RootDiskUnreadable => {
                "Delete the workspace and create it again to give it a fresh root disk; the \
                 volume with your work can be kept."
            }
        }
    }
}

#[cfg(test)]
mod tests {
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
                    cause: StartRefusal::RootDiskOutsideFolder,
                },
                r#"start of sandbox "a" refused, nothing was attached: its root-disk journal names a file outside its own folder"#,
            ),
            (
                ComputeError::StartRefused {
                    sandbox: "a".into(),
                    cause: StartRefusal::RootDiskUnreadable,
                },
                "cannot be read, so the files the start would attach are unknown",
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
}
