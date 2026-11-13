// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspace service: the trait the API calls and the stand-in for a host without workspaces.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_types::{EntryId, SessionUser, WorkspaceId};

use crate::checkouts::{AddRepository, Checkouts};

use super::{
    AttachMode, Attached, DeleteCheck, DisplayName, GitSetup, LocalSource, NewWorkspace, RepoUrl,
    StartFolder, StartFolderChange, WorkspaceRecord,
};

/// Why a workspace call failed. Messages are for the user: lower case, no trailing period.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceError {
    /// No such workspace.
    #[error("{0}")]
    NotFound(String),
    /// The workspace is in a state that doesn't allow this: running, busy, name taken, or
    /// changed since the user looked.
    #[error("{0}")]
    Conflict(String),
    /// A value was refused.
    #[error("{0}")]
    Invalid(String),
    /// The service can't do it now (no runtime, not wired in).
    #[error("{0}")]
    Unavailable(String),
    /// Puddle failed; the text goes to the log, not to the client.
    #[error("{0}")]
    Internal(String),
}

/// What running a shell init in a throwaway shell in the workspace showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShellInitCheck {
    /// It parsed and printed nothing.
    Passed,
    /// It was not run, for the reason given (the workspace is stopped, say).
    NotRun(String),
    /// It does not parse, or printed something, or ran too long: the text says what, for the user.
    Refused(String),
}

/// A service Puddle keeps a login for in a workspace: names and a time, never a value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptLogin {
    /// The service as the API names it (`claude`, `github`): what forgetting takes.
    pub service: String,
    /// What the user calls it (`Claude Code`).
    pub name: String,
    /// Epoch ms Puddle first noted the login as kept; `None` when it does not know.
    pub since: Option<u64>,
}

/// The logins kept for a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptLogins {
    /// The services with a login kept.
    pub logins: Vec<KeptLogin>,
    /// `false` when the credential store did not answer and Puddle had no record of its own: the
    /// list is empty because it is not known.
    pub store_readable: bool,
}

/// The workspaces resource. Implemented by the application over the runtime, and by
/// [`crate::FakeWorkspaces`].
///
/// Contract:
///
/// - Calls return after the request is checked and accepted, not after the work: the record they
///   return has [`WorkspaceRecord::busy`] set. Progress and the end of the operation arrive as
///   events; a failure is a [`puddle_types::WorkspaceStep::Failed`] event, not a late error.
/// - [`WorkspaceError::Conflict`] when the workspace is busy or already in the wanted state.
/// - [`WorkspaceService::delete`] never removes work the user hasn't seen: see its docs.
pub trait WorkspaceService: Send + Sync {
    /// Every workspace, ordered by id.
    fn list(&self) -> BoxFuture<'_, Result<Vec<WorkspaceRecord>, WorkspaceError>>;

    /// One workspace.
    fn get<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Creates a workspace: its volume, then a clone of each repository, one at a time. Once the
    /// create is accepted and before the first clone it runs `git` for the new repositories.
    /// [`WorkspaceError::Conflict`] if the name is taken. A clone that fails fails only its
    /// repository (a background problem names it); the create still ends done.
    fn create<'a>(
        &'a self,
        new: NewWorkspace,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Looks at the folder `path` on this computer as a checkout to start a workspace from
    /// (blocking: it reads the file system). The default says it is not available.
    ///
    /// # Errors
    ///
    /// Why the folder can't be used, for the user.
    fn local_checkout(&self, path: &str) -> Result<LocalSource, String> {
        let _ = path;
        Err("starting from a checkout on this computer is not available here".into())
    }

    /// Boots a workspace that is down.
    fn start<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Shuts a running workspace down.
    fn stop<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Gives freed disk space back to the host.
    fn reclaim<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Removes the stopped workspace's sandbox, and with it the root disk (home directory, tool
    /// caches, installed packages); the next start makes a fresh one. The volume is untouched.
    fn reset_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Makes the workspace's root disk `size_mib` big. A root disk only grows, so the size must
    /// be above the current one. A running workspace has the room at once when the runtime can
    /// grow a disk under a running guest, and is otherwise restarted (nothing is lost); a
    /// stopped one has it at its next start. The size is kept: a reset of the root disk makes
    /// the fresh one this big. Refused when the added room is more than the computer has free.
    fn grow_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
        size_mib: u32,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Looks for what deleting the workspace would lose. Works on a running or a stopped
    /// workspace.
    fn delete_check<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<DeleteCheck, WorkspaceError>>;

    /// Deletes a workspace that is stopped. It checks again first: with `fingerprint` it goes
    /// ahead only if the fresh check has exactly that fingerprint (the user saw this report);
    /// without one it goes ahead only if the fresh check is clean. Otherwise
    /// [`WorkspaceError::Conflict`] and nothing is deleted.
    fn delete<'a>(
        &'a self,
        id: &'a WorkspaceId,
        fingerprint: Option<&'a str>,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Renames the workspace. Only the name changes: the id, the volume and the sandbox stay,
    /// so this works on a running workspace and does not restart anything. [`WorkspaceError::Conflict`] when another
    /// workspace already goes by that name (ignoring case), or while this one is being created
    /// or deleted.
    fn rename<'a>(
        &'a self,
        id: &'a WorkspaceId,
        name: DisplayName,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Sets where new terminals land and what the editor opens: a path under the volume, or
    /// `None` for the volume root. The folder need not exist yet. In a running workspace the
    /// login-shell file follows at once and the answer says whether the folder exists; the
    /// editor's server picks the folder at its next start. [`WorkspaceError::Conflict`] while the
    /// workspace is being created or deleted.
    fn set_start_folder<'a>(
        &'a self,
        id: &'a WorkspaceId,
        start_folder: Option<StartFolder>,
    ) -> BoxFuture<'a, Result<StartFolderChange, WorkspaceError>>;

    /// Sets the guest user the workspace's sessions, browser editor and clones run as. It takes
    /// effect at the next start: a running guest keeps the user it booted with.
    /// [`WorkspaceError::Conflict`] while the workspace is being created or deleted.
    fn set_session_user<'a>(
        &'a self,
        id: &'a WorkspaceId,
        user: SessionUser,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>>;

    /// Opens the workspace in an editor. The workspace must be running.
    fn attach<'a>(
        &'a self,
        id: &'a WorkspaceId,
        mode: AttachMode,
    ) -> BoxFuture<'a, Result<Attached, WorkspaceError>>;

    /// Runs a shell init `script` once in a throwaway shell in the running workspace `id` and says
    /// whether it parsed and printed nothing. The default does not check: a build with no guest
    /// to run it in.
    fn check_shell_init<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _script: &'a str,
    ) -> BoxFuture<'a, Result<ShellInitCheck, WorkspaceError>> {
        Box::pin(async {
            Ok(ShellInitCheck::NotRun(
                "this Puddle has no workspace to run it in".to_owned(),
            ))
        })
    }

    /// The logins Puddle keeps for workspace `id`, from its own record of them (the credential
    /// store is asked only when there is none yet), so a locked store does not stall the answer.
    ///
    /// # Errors
    /// [`WorkspaceError::NotFound`] for no such workspace, [`WorkspaceError::Unavailable`] when
    /// this Puddle keeps no logins.
    fn kept_logins<'a>(
        &'a self,
        _id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<KeptLogins, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "this Puddle keeps no logins".to_owned(),
            ))
        })
    }

    /// Forgets the login Puddle kept for `service` (`claude`, `github`) in workspace `id`: the
    /// real tokens leave the credential store, and a notice that the login is kept but unused
    /// ends with it. The workspace's own copy is only a stand-in and is left alone.
    ///
    /// # Errors
    /// [`WorkspaceError::NotFound`] for no such workspace or no such service,
    /// [`WorkspaceError::Unavailable`] when the credential store refused a deletion (what was
    /// deleted stays deleted; ask again after it answers).
    fn forget_login<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _service: &'a str,
    ) -> BoxFuture<'a, Result<(), WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "this Puddle keeps no logins".to_owned(),
            ))
        })
    }

    /// The workspace disks Puddle keeps whose workspace its list does not name, in id order. The
    /// list is read fresh each time, so a disk a workspace has since claimed is not in it. Puddle
    /// cannot tell from outside whether such a disk holds any files.
    ///
    /// # Errors
    /// [`WorkspaceError::Unavailable`] when the runtime cannot list its disks.
    fn orphan_volumes(&self) -> BoxFuture<'_, Result<Vec<WorkspaceId>, WorkspaceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Adds a list entry for the unclaimed workspace disk `id`, named after the disk. The entry's
    /// repositories are the checkouts on the disk that have a readable remote address (so the
    /// disk has to be read in a short-lived maintenance sandbox); when there is none, `repository`
    /// is the address to use. Nothing is written to the disk, and an entry that already exists is
    /// never replaced. The new workspace is stopped.
    ///
    /// # Errors
    /// [`WorkspaceError::NotFound`] when the disk does not exist,
    /// [`WorkspaceError::Conflict`] when the list already names the workspace or something holds
    /// the disk, [`WorkspaceError::Invalid`] when no address could be read and none was given,
    /// [`WorkspaceError::Unavailable`] when the list cannot be saved.
    fn restore_orphan<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _repository: Option<RepoUrl>,
        _git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async {
            Err(WorkspaceError::Unavailable(
                "this Puddle keeps no workspace disks".to_owned(),
            ))
        })
    }

    /// The checkouts under the workspace's volume root, each with the list entry that has its
    /// directory. A running workspace lists them now (calls at the same
    /// moment share one listing); a stopped one answers the last listing this run made, or says
    /// to start it. A listing that cannot be made or read is a [`Checkouts::problem`], not an
    /// error.
    fn checkouts<'a>(
        &'a self,
        _id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<Checkouts, WorkspaceError>> {
        unavailable()
    }

    /// Adds a repository to a running workspace's list. A clone is
    /// accepted like any operation: the record returned is busy `cloning`. Managing a checkout
    /// that is already on the volume answers at once with the changed record. `git` sets up the
    /// repository's identity and table row once the request is accepted and before any clone.
    /// [`WorkspaceError::Conflict`] when the workspace is not running or busy, the list is full,
    /// the repository or directory is listed, the directory exists (clone) or is no checkout or
    /// has no usable remote (existing); [`WorkspaceError::NotFound`] for an unknown workspace.
    fn add_repository<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _add: AddRepository,
        _git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }

    /// Clones the list entry `_entry`, whose directory is not on the volume, again. Accepted
    /// like [`WorkspaceService::add_repository`] with a clone.
    fn clone_again<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _entry: &'a EntryId,
        _git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }

    /// Takes an entry off the list and nothing else: the checkout, the
    /// repository table row and the identities stay. [`WorkspaceError::NotFound`] for an unknown
    /// entry; [`WorkspaceError::Conflict`] while the workspace is being created or deleted.
    fn remove_repository<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _entry: &'a EntryId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }

    /// Tells the service that settings changed, so it can apply what takes effect at once (the
    /// SSH endpoint of a running workspace opens or closes with its `direct_ssh` setting).
    /// The default does nothing.
    fn settings_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// The lock the settings routes hold from reading a settings document to writing it, when the
    /// service also writes settings documents itself (a delete removes a workspace's): it takes
    /// the same lock, so the two never interleave. `None` when it writes none.
    fn settings_lock(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        None
    }
}

/// The service when none is wired in: every call says so. The API uses it until the
/// application provides the real one.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoWorkspaces;

pub(crate) fn unavailable<T: Send + 'static>() -> BoxFuture<'static, Result<T, WorkspaceError>> {
    Box::pin(async {
        Err(WorkspaceError::Unavailable(
            "workspaces are not available in this build yet".into(),
        ))
    })
}

impl WorkspaceService for NoWorkspaces {
    fn list(&self) -> BoxFuture<'_, Result<Vec<WorkspaceRecord>, WorkspaceError>> {
        unavailable()
    }
    fn get<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn create<'a>(
        &'a self,
        _: NewWorkspace,
        _: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn start<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn stop<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn reclaim<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn reset_root_disk<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn grow_root_disk<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: u32,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn delete_check<'a>(
        &'a self,
        _: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<DeleteCheck, WorkspaceError>> {
        unavailable()
    }
    fn delete<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: Option<&'a str>,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn rename<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: DisplayName,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn set_start_folder<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: Option<StartFolder>,
    ) -> BoxFuture<'a, Result<StartFolderChange, WorkspaceError>> {
        unavailable()
    }
    fn set_session_user<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: SessionUser,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }
    fn attach<'a>(
        &'a self,
        _: &'a WorkspaceId,
        _: AttachMode,
    ) -> BoxFuture<'a, Result<Attached, WorkspaceError>> {
        unavailable()
    }
}
