// SPDX-License-Identifier: GPL-3.0-or-later
//! Why a workspace operation (create, start, stop, delete and the steps they share) failed.
//!
//! The text of an error is what the user is shown (the operation's last event, the audit log and
//! the problem lists), so each variant's message is for the user: lower case, no trailing period
//! and the next step where there is one. A caller that needs to tell causes apart matches on the
//! variant; one that only reports calls `to_string`.

use puddle_api::WorkspaceError;
use puddle_types::WorkspaceId;

use crate::boot::BootInputError;
use crate::injection::InjectionError;
use crate::settings_read::SettingsReadError;
use crate::shell_init::ShellInitError;
use crate::ssh_config::SshConfigError;

/// The way out after the workspace list could not be saved; the error already names the file.
pub(crate) const SAVE_HINT: &str =
    "check the free disk space and that Puddle may write that file, then try again";

/// Why a step of a workspace operation failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpError {
    /// The workspace is not in the list.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    /// Direct SSH is off for the workspace.
    #[error("direct SSH is off for this workspace; allow it first")]
    DirectSshOff,
    /// The settings that say whether direct SSH is on cannot be read.
    #[error("the settings cannot be read ({0}); direct SSH counts as off until they can")]
    DirectSshUnknown(#[source] SettingsReadError),
    /// Direct SSH has no endpoint to name.
    #[error("direct SSH has no endpoint in {0}; restart the workspace to try again")]
    NoEndpoint(WorkspaceId),
    /// The ssh config entry could not be written.
    #[error("direct SSH is on but the ssh config could not be written: {0}")]
    SshEntry(#[source] SshConfigError),
    /// The checkouts can only be listed in a running workspace.
    #[error("the workspace is not running; start it to see its checkouts")]
    NoCheckoutsListed,
    /// The workspace has no running sandbox.
    #[error("{0} is not running")]
    NotRunning(WorkspaceId),
    /// The host is closing.
    #[error("Puddle is shutting down")]
    ShuttingDown,
    /// The workspace list could not be saved.
    #[error(transparent)]
    Persist(#[from] crate::HostError),
    /// A settings document could not be used.
    #[error(transparent)]
    Settings(#[from] SettingsReadError),
    /// The proxy's per-workspace state could not be made or changed.
    #[error(transparent)]
    Injection(#[from] InjectionError),
    /// The boot inputs could not be made.
    #[error(transparent)]
    Boot(#[from] BootInputError),
    /// The ssh config could not be written.
    #[error(transparent)]
    Ssh(#[from] SshConfigError),
    /// The shell init could not be saved, copied or checked.
    #[error(transparent)]
    ShellInit(#[from] ShellInitError),
    /// The boot plan could not be run in the guest.
    #[error(transparent)]
    Run(#[from] puddle_boot::BootFailure),
    /// The boot hook failed.
    #[error(transparent)]
    Hook(#[from] puddle_boot::BootError),
    /// The workspace's volume or checkouts could not be worked on.
    #[error(transparent)]
    Volume(#[from] puddle_workspace::WorkspaceError),
    /// A stored value is not valid any more.
    #[error(transparent)]
    Invalid(#[from] puddle_types::ValidationError),
    /// The sandbox has no route registered.
    #[error("{0} has no route")]
    NoRoute(WorkspaceId),
    /// The step failed, and some of what it made could not be taken down.
    #[error(
        "{cause}; Puddle could not clean up after it: {leftovers}. Puddle removes a sandbox no \
         workspace claims at its next start, and keeps a volume it does not know, listing it then"
    )]
    Cleanup {
        /// Why the step failed.
        cause: Box<OpError>,
        /// What is left, one phrase each, joined with `; `.
        leftovers: String,
    },
    /// The step failed and the list could not be saved either.
    #[error("{cause}; {source}; {SAVE_HINT}")]
    NotSaved {
        /// Why the step failed.
        cause: Box<OpError>,
        /// Why the save failed.
        source: crate::HostError,
    },
    /// The list could not be saved after the step.
    #[error("{source}; {SAVE_HINT}")]
    SaveFailed {
        /// Why the save failed.
        source: crate::HostError,
    },
    /// The list could not be saved, so the new workspace was taken down again.
    #[error("{source}; the new workspace was removed again; {SAVE_HINT}")]
    NewNotSaved {
        /// Why the save failed.
        source: crate::HostError,
    },
    /// The runtime refused or failed.
    #[error(transparent)]
    Runtime(#[from] puddle_compute::ComputeError),
}
