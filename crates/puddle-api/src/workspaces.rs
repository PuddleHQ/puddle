// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspaces the API serves: the [`WorkspaceService`] trait the routes call, the domain
//! types it speaks, and the [`Launcher`] that opens a desktop editor.
//!
//! The routes know nothing about volumes, workspaces or the runtime. The real service (over
//! `puddle-workspace` and `puddle-lifecycle`) is wired in by the application; [`crate::FakeWorkspaces`]
//! is the in-memory one for tests and the UI fixture. Every method returns promptly: a long
//! operation (create, start, stop, reclaim, delete) is *accepted* by the call, returns the
//! workspace with [`WorkspaceRecord::busy`] set, and reports the rest through
//! [`puddle_types::Event::StatusChanged`] and [`puddle_types::Event::WorkspaceProgress`].

use std::fmt;

use futures_util::future::BoxFuture;
use puddle_store::UserId;
use puddle_types::{
    CheckoutDir, DiskKind, EntryId, ImageRef, MemoryMib, SandboxName, SessionUser, WorkspaceId,
    WorkspaceStatus,
};
use sha2::{Digest, Sha256};

mod launch;
mod repositories;
mod service;

pub use launch::{
    BrowserTarget, CONNECTION_TOKEN_COOKIE, DesktopTarget, EditorLogin, EditorSignIn, LaunchError,
    Launcher, OpenTarget,
};
pub use repositories::{
    CheckoutProblem, EntryProblem, LocalCheckoutError, LocalSource, MAX_REPO_URL_LEN,
    MAX_REPOSITORIES, NewRepository, NoFreeEntryId, RepoUrl, RepoUrlError, Repository,
    RepositoryDraft, RepositoryListError, check_repositories, check_repositories_with,
    new_entry_id,
};
pub use service::{
    KeptLogin, KeptLogins, NoWorkspaces, ShellInitCheck, WorkspaceError, WorkspaceService,
};

/// The long operation a workspace is in the middle of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Operation {
    /// Creating the volume and cloning the repositories.
    Creating,
    /// Booting.
    Starting,
    /// Shutting down.
    Stopping,
    /// Giving freed disk space back to the host.
    Reclaiming,
    /// Removing the sandbox so the next start makes a fresh root disk.
    ResettingRootDisk,
    /// Making the root disk bigger.
    GrowingRootDisk,
    /// Cloning one repository into a running workspace.
    Cloning,
    /// Checking for unsaved work and removing the workspace.
    Deleting,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Creating => "creating",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
            Self::Reclaiming => "reclaiming",
            Self::ResettingRootDisk => "resetting_root_disk",
            Self::GrowingRootDisk => "growing_root_disk",
            Self::Cloning => "cloning",
            Self::Deleting => "deleting",
        })
    }
}

/// One workspace as the service reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkspaceRecord {
    /// The workspace's id (its volume is `ws-<id>`).
    pub id: WorkspaceId,
    /// What the user calls the workspace: display text. Nothing else (sandbox, volume, rules,
    /// settings, events) uses it; the id does.
    pub name: DisplayName,
    /// The repositories Puddle manages for it, in the order they were added.
    pub repositories: Vec<Repository>,
    /// Where new terminals land and what the editor opens; `None` is the volume root.
    pub start_folder: Option<StartFolder>,
    /// The guest user its sessions, browser editor and clones run as, from the next start.
    pub session_user: SessionUser,
    /// The image its workspace boots.
    pub image: String,
    /// Its memory, applied at the next start.
    pub memory: MemoryMib,
    /// The workspace's state.
    pub status: WorkspaceStatus,
    /// The operation in progress, if any.
    pub busy: Option<Operation>,
    /// Epoch ms it was created.
    pub created_at: u64,
    /// The volume's size in MiB: the most the workspace can hold.
    pub disk_size_mib: u64,
    /// What the volume holds now in MiB, when known.
    pub disk_used_mib: Option<u64>,
    /// The root disk's size in MiB, when the runtime can tell. The root disk holds the home
    /// directory, tool caches and installed packages, and goes with the sandbox.
    pub root_disk_size_mib: Option<u64>,
    /// What the root disk holds now in MiB, when known.
    pub root_disk_used_mib: Option<u64>,
    /// The size in MiB the user grew the root disk to, kept so a reset makes the fresh root disk
    /// this big; `None` when it was never grown.
    pub root_disk_grown_mib: Option<u32>,
}

impl WorkspaceRecord {
    /// A record with the defaults the fake and the tests start from.
    #[must_use]
    pub fn new(id: WorkspaceId, name: DisplayName, repositories: Vec<Repository>) -> Self {
        Self {
            id,
            name,
            repositories,
            start_folder: None,
            session_user: SessionUser::Agent,
            image: DEFAULT_IMAGE.to_owned(),
            memory: MemoryMib::DEFAULT,
            status: WorkspaceStatus::Created,
            busy: None,
            created_at: 0,
            disk_size_mib: DEFAULT_DISK_MIB,
            disk_used_mib: None,
            root_disk_size_mib: None,
            root_disk_used_mib: None,
            root_disk_grown_mib: None,
        }
    }
}

impl WorkspaceRecord {
    /// How much of the `disk` is used, in whole percent rounded down and at most 100; `None`
    /// when the use or the size is not known, or the size is zero.
    #[must_use]
    pub fn disk_percent(&self, disk: DiskKind) -> Option<u8> {
        let (used, size) = match disk {
            DiskKind::Root => (self.root_disk_used_mib?, self.root_disk_size_mib?),
            DiskKind::Volume => (self.disk_used_mib?, self.disk_size_mib),
        };
        if size == 0 {
            return None;
        }
        let percent = (u128::from(used) * 100 / u128::from(size)).min(100);
        u8::try_from(percent).ok()
    }

    /// The disks at or over `threshold`, the root disk first. A disk whose use is not known is
    /// not counted.
    #[must_use]
    pub fn disk_warnings(&self, threshold: puddle_settings::DiskWarningPercent) -> Vec<DiskKind> {
        [DiskKind::Root, DiskKind::Volume]
            .into_iter()
            .filter(|disk| {
                let (used, size) = match disk {
                    DiskKind::Root => (self.root_disk_used_mib, self.root_disk_size_mib),
                    DiskKind::Volume => (self.disk_used_mib, Some(self.disk_size_mib)),
                };
                matches!((used, size), (Some(used), Some(size)) if threshold.reached(used, size))
            })
            .collect()
    }
}

impl WorkspaceRecord {
    /// What the user calls the workspace: its name.
    #[must_use]
    pub fn label(&self) -> &str {
        self.name.as_str()
    }
}

/// Why a workspace name is refused. The messages are for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DisplayNameError {
    /// Nothing but white space.
    #[error("the name must not be empty")]
    Empty,
    /// More than [`DisplayName::MAX_CHARS`] characters.
    #[error("the name is longer than {} characters", DisplayName::MAX_CHARS)]
    TooLong,
    /// A control character, a line break included.
    #[error("the name must not contain line breaks or control characters")]
    ControlCharacter,
}

/// A workspace's name: free text of 1 to [`DisplayName::MAX_CHARS`]
/// characters, trimmed, with no control characters (a line break included).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DisplayName(String);

impl DisplayName {
    /// Longest display name, in characters.
    pub const MAX_CHARS: usize = 60;

    /// Checks a display name.
    ///
    /// # Errors
    ///
    /// [`DisplayNameError`] when the name is empty, too long or has control characters.
    pub fn parse(text: &str) -> Result<Self, DisplayNameError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(DisplayNameError::Empty);
        }
        if text.chars().count() > Self::MAX_CHARS {
            return Err(DisplayNameError::TooLong);
        }
        if text.chars().any(char::is_control) {
            return Err(DisplayNameError::ControlCharacter);
        }
        Ok(Self(text.to_owned()))
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The workspace in `records` that already goes by `label`, ignoring case and `except` itself.
/// Two workspaces never share a label, so the list, the dialogs and the toasts stay unambiguous.
pub fn label_taken<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord>,
    label: &str,
    except: Option<&WorkspaceId>,
) -> Option<&'a WorkspaceRecord> {
    let wanted = label.to_lowercase();
    records
        .into_iter()
        .find(|r| Some(&r.id) != except && r.label().to_lowercase() == wanted)
}

/// Whether the workspace `id` may take `name` among `records`. The one rule both services apply.
///
/// # Errors
///
/// [`WorkspaceError::NotFound`] for an unknown id; [`WorkspaceError::Conflict`] while the
/// workspace is being created or deleted (there is nothing settled to rename), or when another
/// workspace already goes by that name.
pub fn check_rename<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord> + Clone,
    id: &WorkspaceId,
    name: &DisplayName,
) -> Result<(), WorkspaceError> {
    let record = records
        .clone()
        .into_iter()
        .find(|r| &r.id == id)
        .ok_or_else(|| WorkspaceError::NotFound(format!("no workspace {id}")))?;
    if let Some(busy @ (Operation::Creating | Operation::Deleting)) = record.busy {
        return Err(WorkspaceError::Conflict(format!(
            "{} is {busy}; rename it when that is done",
            record.label()
        )));
    }
    match label_taken(records, name.as_str(), Some(id)) {
        Some(other) => Err(WorkspaceError::Conflict(format!(
            "another workspace is already called {}; choose a different name",
            other.label()
        ))),
        None => Ok(()),
    }
}

/// Which workspaces `query` names, for a person who types text: the one whose id
/// is exactly `query`, and the ones whose name equals it ignoring case and the spaces at the
/// ends. A workspace whose id and name both match counts once. At most one match is the normal
/// case; two happen only when one workspace's id is another's name (a legacy id).
#[must_use]
pub(crate) fn lookup<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord>,
    query: &str,
) -> Vec<&'a WorkspaceRecord> {
    let wanted = query.trim().to_lowercase();
    records
        .into_iter()
        .filter(|r| r.id.as_str() == query || r.name.as_str().to_lowercase() == wanted)
        .collect()
}

/// The image a workspace gets when the request names none, pinned by digest so a registry change reaches users
/// only through a reviewed update (`ci/image-pins.sh`).
pub const DEFAULT_IMAGE: &str = "mcr.microsoft.com/devcontainers/base:debian@sha256:71b6fe7c4e07ec747a94a72b2a17f2f8c286eaa7b1c59862888df764efb8b6c6";

/// The volume size a new workspace gets (32 GiB, sparse on the host apart from the 64 MiB journal
/// written when the volume is made).
pub const DEFAULT_DISK_MIB: u64 = 32 * 1024;

/// The root disk size the fake workspaces show: msb's default writable layer.
pub const DEFAULT_ROOT_DISK_MIB: u64 = 4 * 1024;

/// Whether the start folder of workspace `id` may change now.
///
/// # Errors
///
/// [`WorkspaceError::NotFound`] for an unknown id; [`WorkspaceError::Conflict`] while the
/// workspace is being created or deleted (there is nothing settled to change).
pub fn check_start_folder_change<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord>,
    id: &WorkspaceId,
) -> Result<(), WorkspaceError> {
    check_settled(records, id, "its start folder")
}

/// Whether the session user of workspace `id` may change now; the same rule as
/// [`check_start_folder_change`].
///
/// # Errors
///
/// As [`check_start_folder_change`].
pub fn check_session_user_change<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord>,
    id: &WorkspaceId,
) -> Result<(), WorkspaceError> {
    check_settled(records, id, "the user it runs as")
}

fn check_settled<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord>,
    id: &WorkspaceId,
    what: &str,
) -> Result<(), WorkspaceError> {
    let record = records
        .into_iter()
        .find(|r| &r.id == id)
        .ok_or_else(|| WorkspaceError::NotFound(format!("no workspace {id}")))?;
    match record.busy {
        Some(busy @ (Operation::Creating | Operation::Deleting)) => {
            Err(WorkspaceError::Conflict(format!(
                "{} is {busy}; change {what} when that is done",
                record.label()
            )))
        }
        _ => Ok(()),
    }
}

/// What setting the start folder did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartFolderChange {
    /// The workspace as it is now.
    pub record: WorkspaceRecord,
    /// Whether the folder exists in the running workspace; `None` when the workspace does not run
    /// (nothing can be looked at) or the start folder is the volume root.
    pub exists: Option<bool>,
}

impl StartFolderChange {
    /// A change to `record`, with whether the folder exists.
    #[must_use]
    pub fn new(record: WorkspaceRecord, exists: Option<bool>) -> Self {
        Self { record, exists }
    }
}

/// Why a start folder is refused. The messages are for the user and name the rule broken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StartFolderError {
    /// An empty path.
    #[error("the start folder must not be empty; use null for the volume root")]
    Empty,
    /// Longer than [`StartFolder::MAX_BYTES`].
    #[error("the start folder is longer than {} bytes", StartFolder::MAX_BYTES)]
    TooLong,
    /// A leading `/`.
    #[error("the start folder is a path under the volume, without a leading /")]
    Absolute,
    /// A control character or a backslash.
    #[error("the start folder must not contain control characters or \\")]
    ControlCharacter,
    /// An empty, `.` or `..` part.
    #[error("the start folder has an empty, . or .. part ({path}); name each folder")]
    BadPart {
        /// The path as given.
        path: String,
    },
    /// Inside a folder Puddle keeps for itself.
    #[error("the start folder must not be inside {folder}")]
    Reserved {
        /// The reserved first part.
        folder: String,
    },
}

/// A start folder: a relative path under the volume root, `/` between its
/// parts, no part empty, `.` or `..`, no control character or `\`, not inside `.puddle` or
/// `lost+found`, at most [`StartFolder::MAX_BYTES`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct StartFolder(String);

impl StartFolder {
    /// Longest start folder, in bytes.
    pub const MAX_BYTES: usize = 255;

    /// Checks a start folder.
    ///
    /// # Errors
    ///
    /// [`StartFolderError`], which says which rule the path breaks.
    pub fn parse(path: &str) -> Result<Self, StartFolderError> {
        if path.is_empty() {
            return Err(StartFolderError::Empty);
        }
        if path.len() > Self::MAX_BYTES {
            return Err(StartFolderError::TooLong);
        }
        if path.starts_with('/') {
            return Err(StartFolderError::Absolute);
        }
        if path.chars().any(|c| c.is_control() || c == '\\') {
            return Err(StartFolderError::ControlCharacter);
        }
        for part in path.split('/') {
            if matches!(part, "" | "." | "..") {
                return Err(StartFolderError::BadPart {
                    path: path.to_owned(),
                });
            }
        }
        let first = path.split('/').next().unwrap_or_default();
        if first == ".puddle" || first == CheckoutDir::LOST_AND_FOUND {
            return Err(StartFolderError::Reserved {
                folder: first.to_owned(),
            });
        }
        Ok(Self(path.to_owned()))
    }

    /// The start folder of a checkout's directory.
    #[must_use]
    pub fn of_checkout(dir: &CheckoutDir) -> Self {
        Self(dir.as_str().to_owned())
    }

    /// The path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StartFolder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How many ids a create draws before it refuses: with about 39 random bits, only a broken
/// random source makes 16 draws in a row hit ids that are taken.
pub const MAX_ID_DRAWS: u32 = 16;

/// Why a create got no id: every draw was taken or gave nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no free workspace id was found; the system's random source may be broken")]
#[non_exhaustive]
pub struct NoFreeId;

/// Why an [`IdSource`] gave no id for one draw. That draw counts, and the create draws again.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IdDrawError {
    /// The operating system's random source failed.
    #[error("no randomness: {0}")]
    Random(getrandom::Error),
    /// The text made from the name is not an id.
    #[error(transparent)]
    Invalid(#[from] puddle_types::ValidationError),
}

/// Where new workspace ids come from.
pub trait IdSource: Send + Sync {
    /// Draw number `attempt` (counted from 0) for a new workspace called `name`. A create
    /// draws again while the id is taken, at most [`MAX_ID_DRAWS`] times.
    ///
    /// # Errors
    ///
    /// [`IdDrawError`] when the source gave nothing. That draw counts.
    fn draw(&self, name: &DisplayName, attempt: u32) -> Result<WorkspaceId, IdDrawError>;
}

/// The operating system's random source, five bytes a draw ([`WorkspaceId::from_random`]);
/// nothing of the name goes into the id. The host uses this one.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds;

impl IdSource for RandomIds {
    fn draw(&self, _name: &DisplayName, _attempt: u32) -> Result<WorkspaceId, IdDrawError> {
        let mut bytes = [0_u8; 5];
        getrandom::fill(&mut bytes).map_err(IdDrawError::Random)?;
        Ok(WorkspaceId::from_random(bytes))
    }
}

/// Ids made from the name, for tests and the UI fixture, which name a workspace's paths before
/// it exists: `Shop API` gets `shop-api` (ASCII letters and digits lower-cased, every other run
/// of characters one hyphen), and the draws after the first add `-2`, `-3` and so on. A name
/// with nothing usable in it, or one that comes out reserved, gets `workspace-` and eight hex
/// digits of a hash of the name.
#[derive(Debug, Clone, Copy, Default)]
pub struct NameIds;

impl IdSource for NameIds {
    fn draw(&self, name: &DisplayName, attempt: u32) -> Result<WorkspaceId, IdDrawError> {
        let base = id_for_name(name)?;
        if attempt == 0 {
            return Ok(base);
        }
        let suffix = format!("-{}", attempt + 1);
        let room = WorkspaceId::MAX_LEN - suffix.len();
        let stem: String = base.as_str().chars().take(room).collect();
        Ok(WorkspaceId::new(&format!(
            "{}{suffix}",
            stem.trim_end_matches('-')
        ))?)
    }
}

/// An entry id made from `text` (a hash of it), for tests and the UI fixture, which need the same
/// entry to get the same id on every run. Nothing in it spells the text.
#[must_use]
pub fn entry_id_from_text(text: &str) -> EntryId {
    let mut bytes = [0_u8; 5];
    for (slot, byte) in bytes.iter_mut().zip(Sha256::digest(text.as_bytes())) {
        *slot = byte;
    }
    EntryId::from_random(bytes)
}

/// The name as an id, for [`NameIds`].
fn id_for_name(name: &DisplayName) -> Result<WorkspaceId, puddle_types::ValidationError> {
    let mut slug = String::new();
    for c in name.as_str().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.truncate(WorkspaceId::MAX_LEN);
    let slug = slug.trim_matches('-');
    if let Ok(id) = WorkspaceId::new(slug) {
        return Ok(id);
    }
    let digest = Sha256::digest(name.as_str().to_lowercase().as_bytes());
    let hex = digest.iter().take(4).fold(String::new(), |mut out, b| {
        let _ = fmt::Write::write_fmt(&mut out, format_args!("{b:02x}"));
        out
    });
    WorkspaceId::new(&format!("workspace-{hex}"))
}

/// What `create` needs. Build it from a request with [`check_repositories`] and the typed names.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewWorkspace {
    /// Whose it is: recorded before the workspace is listed, saved or announced anywhere, and
    /// never changed after.
    pub owner: UserId,
    /// The workspace's name: display text. The id is drawn apart from it (see [`NewWorkspace::id`]).
    pub name: DisplayName,
    /// What to clone, in order, with the Git settings each starts with.
    pub repositories: Vec<NewRepository>,
    /// The start folder; `None` is the volume root.
    pub start_folder: Option<StartFolder>,
    /// The image, or the default.
    pub image: Option<ImageRef>,
    /// The memory, or the default.
    pub memory: Option<MemoryMib>,
}

impl NewWorkspace {
    /// A request for a workspace of `owner` with the defaults.
    #[must_use]
    pub fn new(owner: UserId, name: DisplayName, repositories: Vec<NewRepository>) -> Self {
        Self {
            owner,
            name,
            repositories,
            start_folder: None,
            image: None,
            memory: None,
        }
    }

    /// The id the new workspace gets: the first draw of `source` that `taken` does not refuse.
    /// Nothing about the name decides it (unless `source` is made to).
    ///
    /// # Errors
    ///
    /// [`NoFreeId`] after [`MAX_ID_DRAWS`] draws that were all taken or gave nothing.
    pub fn id(
        &self,
        source: &dyn IdSource,
        taken: impl Fn(&WorkspaceId) -> bool,
    ) -> Result<WorkspaceId, NoFreeId> {
        for attempt in 0..MAX_ID_DRAWS {
            match source.draw(&self.name, attempt) {
                Ok(id) if !taken(&id) => return Ok(id),
                Ok(id) => tracing::debug!(%id, attempt, "a drawn workspace id is taken"),
                Err(reason) => tracing::warn!(%reason, attempt, "no workspace id was drawn"),
            }
        }
        Err(NoFreeId)
    }

    /// The entries the new workspace's list holds.
    #[must_use]
    pub fn entries(&self) -> Vec<Repository> {
        self.repositories
            .iter()
            .map(|r| r.repository.clone())
            .collect()
    }
}

/// Sets up the Git settings of a new workspace's repositories. A service calls
/// it once a create is accepted (the name is reserved and the workspace list saved) and before
/// the first clone, so every clone carries its credential. It reports nothing back to the
/// service: a repository it could not set up is cloned without, and the caller tells the user.
pub trait GitSetup: Send + Sync {
    /// Sets up `repositories` of `workspace`, in order.
    fn set_up<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
        repositories: &'a [NewRepository],
    ) -> BoxFuture<'a, ()>;

    /// Records that the new workspace `workspace` is `owner`'s ([`NewWorkspace::owner`]). A
    /// create calls it once its id is drawn and before the workspace is listed, saved or announced
    /// anywhere, so nothing about it is ever seen as another user's. A workspace with no recorded
    /// user is nobody's: no user reaches it.
    ///
    /// # Errors
    ///
    /// The record could not be written; the create stops and nothing of it exists.
    fn claim(&self, workspace: &WorkspaceId, owner: UserId) -> Result<(), WorkspaceError>;
}

/// A [`GitSetup`] that sets nothing up, for services and tests with no store.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGitSetup;

impl GitSetup for NoGitSetup {
    fn set_up<'a>(&'a self, _: &'a WorkspaceId, _: &'a [NewRepository]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// No store, so nothing to record: only for a service whose workspaces no user reaches.
    fn claim(&self, _: &WorkspaceId, _: UserId) -> Result<(), WorkspaceError> {
        Ok(())
    }
}

/// A bounded list of lines, as the unsaved-work check reports them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    /// The first items.
    pub items: Vec<String>,
    /// How many more there are.
    pub more: u64,
}

impl Listing {
    /// Whether nothing is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.more == 0
    }
}

/// What one checkout holds that isn't on a remote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoFindings {
    /// The checkout's directory under the workspace.
    pub dir: String,
    /// Uncommitted changes and untracked files.
    pub uncommitted: Listing,
    /// Commits on no remote branch.
    pub unpushed: Listing,
    /// Stashes.
    pub stashes: Listing,
}

impl RepoFindings {
    /// Whether nothing in this checkout would be lost.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.uncommitted.is_empty() && self.unpushed.is_empty() && self.stashes.is_empty()
    }
}

/// What deleting a workspace would lose.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DeleteCheck {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// Every checkout, clean or not.
    pub repos: Vec<RepoFindings>,
    /// Data outside any checkout.
    pub other: Listing,
    /// What could not be checked. A check that could not run counts as unsafe.
    pub errors: Vec<String>,
    /// The stopped workspace that is removed together with the workspace.
    pub removes_sandbox: Option<SandboxName>,
    /// The workspace's volume is already gone: there was nothing to inspect, and deleting loses
    /// nothing; it only drops the workspace.
    pub volume_missing: bool,
    /// A digest of exactly this report. A delete that passes it is refused when the workspace
    /// has changed since.
    pub fingerprint: String,
}

impl DeleteCheck {
    /// A report with its fingerprint computed from the findings, so every service (the real one
    /// and the fake) digests a report the same way.
    #[must_use]
    pub fn new(
        workspace: WorkspaceId,
        repos: Vec<RepoFindings>,
        other: Listing,
        errors: Vec<String>,
        removes_sandbox: Option<SandboxName>,
    ) -> Self {
        let mut check = Self {
            workspace,
            repos,
            other,
            errors,
            removes_sandbox,
            volume_missing: false,
            fingerprint: String::new(),
        };
        check.fingerprint = fingerprint(&check);
        check
    }

    /// Marks the report as one for a workspace whose volume is gone (recomputes the fingerprint,
    /// so a delete confirmed for this report is refused if the volume comes back).
    #[must_use]
    pub fn with_volume_missing(mut self) -> Self {
        self.volume_missing = true;
        self.fingerprint = fingerprint(&self);
        self
    }

    /// Whether deleting loses nothing the check can see.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.repos.iter().all(RepoFindings::is_clean)
            && self.other.is_empty()
            && self.errors.is_empty()
    }
}

/// How to open the workspace in an editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachMode {
    /// VS Code on the desktop, through the [`Launcher`].
    Desktop,
    /// VS Code in the browser, in a window the shell opens.
    Browser,
}

/// The result of an attach.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Attached {
    /// Whether Puddle opened an editor itself (a window or desktop VS Code, when it worked).
    pub opened: bool,
    /// Why nothing was opened, when that is not an error.
    pub message: Option<String>,
    /// What a client on another machine can open to reach the editor itself: the `vscode://` link
    /// of a desktop attach. `None` when there is nothing a client could open (a browser attach).
    pub open_uri: Option<String>,
}

impl Attached {
    /// An editor was opened.
    #[must_use]
    pub fn opened() -> Self {
        Self {
            opened: true,
            message: None,
            open_uri: None,
        }
    }

    /// Nothing was opened, for this reason.
    #[must_use]
    pub fn not_opened(message: impl Into<String>) -> Self {
        Self {
            opened: false,
            message: Some(message.into()),
            open_uri: None,
        }
    }

    /// The same result, naming what a client can open itself.
    #[must_use]
    pub fn with_open_uri(mut self, uri: impl Into<String>) -> Self {
        self.open_uri = Some(uri.into());
        self
    }
}

/// A digest over everything a user sees in the report, so a delete can prove it is for that
/// report.
fn fingerprint(check: &DeleteCheck) -> String {
    let mut hash = Sha256::new();
    let mut put = |text: &str| {
        hash.update(u64::try_from(text.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(text.as_bytes());
    };
    let mut list = |name: &str, l: &Listing| {
        put(name);
        for item in &l.items {
            put(item);
        }
        put(&l.more.to_string());
    };
    for repo in &check.repos {
        list(&format!("dir:{}", repo.dir), &Listing::default());
        list("uncommitted", &repo.uncommitted);
        list("unpushed", &repo.unpushed);
        list("stashes", &repo.stashes);
    }
    list("other", &check.other);
    let mut errors = Listing::default();
    errors.items.clone_from(&check.errors);
    list("errors", &errors);
    if check.volume_missing {
        put("volume_missing");
    }
    hash.finalize()
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

#[cfg(test)]
mod tests;
