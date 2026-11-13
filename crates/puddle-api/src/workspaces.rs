// SPDX-License-Identifier: GPL-3.0-or-later
//! The workspaces the API serves: the [`WorkspaceService`] trait the routes call, the domain
//! types it speaks, and the [`Launcher`] that opens a desktop editor.
//!
//! The routes know nothing about volumes, workspaces or the runtime. The real service (over
//! `puddle-workspace` and `puddle-lifecycle`) is wired in by the application; [`crate::FakeWorkspaces`]
//! is the in-memory one for tests and the UI fixture. Every method returns promptly: a long
//! operation (create, start, stop, reclaim, delete) is *accepted* by the call, returns the
//! workspace with [`WorkspaceRecord::busy`] set, and reports the rest through
//! [`puddle_types::Event::StatusChanged`] and [`puddle_types::Event::WorkspaceProgress`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_store::{IdentityId, RepoRef};
use puddle_types::{
    CheckoutDir, DiskKind, EntryId, ImageRef, MemoryMib, SandboxName, SessionUser, WorkspaceId,
    WorkspaceStatus,
};
use sha2::{Digest, Sha256};

use crate::checkouts::{AddRepository, Checkouts};

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
    /// A message for the user when the name is empty, too long or has control characters.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("the name must not be empty".into());
        }
        if text.chars().count() > Self::MAX_CHARS {
            return Err(format!(
                "the name is longer than {} characters",
                Self::MAX_CHARS
            ));
        }
        if text.chars().any(char::is_control) {
            return Err("the name must not contain line breaks or control characters".into());
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

/// A checked HTTPS repository URL: no credentials, no whitespace, a host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoUrl(String);

/// Longest repository URL accepted.
pub const MAX_REPO_URL_LEN: usize = 2048;

impl RepoUrl {
    /// Checks a repository URL.
    ///
    /// # Errors
    ///
    /// A message for the user: SSH remotes get the "not supported yet" one, anything else that
    /// is not a plain `https://` URL says what to change.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("ssh://") || is_scp_like(url) {
            return Err(
                "SSH remotes are not supported yet; use the repository's HTTPS URL instead".into(),
            );
        }
        if url.len() > MAX_REPO_URL_LEN {
            return Err(format!(
                "the repository URL is longer than {MAX_REPO_URL_LEN} characters"
            ));
        }
        if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err("the repository URL must not contain spaces or control characters".into());
        }
        let Some(rest) = lower.strip_prefix("https://") else {
            return Err("use an https:// URL for the repository".into());
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(
                "remove the user name and password from the URL; Puddle supplies credentials itself"
                    .into(),
            );
        }
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        if host.is_empty() {
            return Err("the repository URL has no host".into());
        }
        Ok(Self(url.to_owned()))
    }

    /// The URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `git@host:owner/repo`-style remotes: no scheme, a `:` before any `/`.
fn is_scp_like(url: &str) -> bool {
    if url.contains("://") || url.to_ascii_lowercase().starts_with("http") {
        return false;
    }
    match (url.find(':'), url.find('/')) {
        // A colon at index 1 is a Windows drive letter, not a host.
        (Some(colon), Some(slash)) => colon > 1 && colon < slash,
        (Some(colon), None) => colon > 1,
        _ => false,
    }
}

impl fmt::Display for RepoUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One repository Puddle manages for a workspace: its entry id, the address it is cloned from
/// and the checkout directly under the volume root it is cloned into. Its identity and its Pull
/// and Push toggles are not part of it: they are the workspace's Git settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// What API paths and problem keys call the entry; the directory is not an id.
    pub entry: EntryId,
    /// Where it is cloned from.
    pub url: RepoUrl,
    /// The checkout it is cloned into.
    pub directory: CheckoutDir,
}

impl Repository {
    /// The entry `entry` for `url` in the directory `git clone` would pick.
    #[must_use]
    pub fn at_default_directory(entry: EntryId, url: RepoUrl) -> Self {
        let directory = CheckoutDir::from_url(url.as_str());
        Self {
            entry,
            url,
            directory,
        }
    }
}

/// A fresh entry id ([`EntryId::from_random`] over the operating system's random source) that
/// `taken` does not know, drawn up to [`MAX_ID_DRAWS`] times; `taken` says whether the entry
/// list of the workspace already has it.
///
/// # Errors
///
/// The message for the user when every draw was taken or the random source gave nothing.
pub fn new_entry_id(taken: impl Fn(&EntryId) -> bool) -> Result<EntryId, String> {
    draw_entry_id(
        |bytes| getrandom::fill(bytes).map_err(|e| e.to_string()),
        taken,
    )
}

/// [`new_entry_id`] over any source of five bytes.
fn draw_entry_id(
    mut fill: impl FnMut(&mut [u8; 5]) -> Result<(), String>,
    taken: impl Fn(&EntryId) -> bool,
) -> Result<EntryId, String> {
    for attempt in 0..MAX_ID_DRAWS {
        let mut bytes = [0_u8; 5];
        match fill(&mut bytes) {
            Ok(()) => {
                let id = EntryId::from_random(bytes);
                if !taken(&id) {
                    return Ok(id);
                }
                tracing::debug!(%id, attempt, "a drawn entry id is taken");
            }
            Err(reason) => tracing::warn!(%reason, attempt, "no entry id was drawn"),
        }
    }
    Err("no free repository entry id was found; the system's random source may be broken".into())
}

/// Most repositories one workspace holds (where the delete check stops counting checkouts).
pub const MAX_REPOSITORIES: usize = 100;

/// A repository a create asks for: the entry, and the Git settings it starts with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRepository {
    /// The entry on the workspace's list.
    pub repository: Repository,
    /// The identity the user picked for it; `None` lets Puddle choose.
    pub identity: Option<IdentityId>,
    /// Pull on its new repository table row.
    pub pull: bool,
    /// Push on its new repository table row.
    pub push: bool,
    /// The `.git` directory of the checkout on this computer it is cloned from (canonical), or
    /// `None` to clone from its address.
    pub local: Option<PathBuf>,
}

/// A checkout on this computer, as the service found it (see
/// [`WorkspaceService::local_checkout`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSource {
    /// The canonical path of its `.git` directory.
    pub git_dir: PathBuf,
    /// The name of its folder, the default directory of its clone.
    pub folder_name: String,
    /// The HTTPS address of its `origin` remote, or why there is none Puddle can use.
    pub origin: Result<String, String>,
}

/// A repository as a request names it, before [`check_repositories`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepositoryDraft {
    /// The address as typed.
    pub url: String,
    /// The checkout's directory as typed; `None` takes the one the address gives.
    pub directory: Option<String>,
    /// The identity picked for it.
    pub identity: Option<IdentityId>,
    /// Pull; `None` is on.
    pub pull: Option<bool>,
    /// Push; `None` is on.
    pub push: Option<bool>,
    /// A checkout on this computer to clone from, as given; [`RepositoryDraft::url`] may then be
    /// empty (the checkout's `origin` is used).
    pub local: Option<String>,
}

/// Checks a create's repository list as a whole, before anything exists. Each
/// refusal names the entry by its place in the list and its address: an address the create
/// checks refuse, a bad directory, two entries with one directory (with a free name to use
/// instead), the same repository twice in the repository table's spelling, an identity
/// `identity_exists` does not know, or more than [`MAX_REPOSITORIES`].
///
/// # Errors
///
/// The message for the user.
pub fn check_repositories(
    drafts: &[RepositoryDraft],
    identity_exists: impl Fn(IdentityId) -> bool,
) -> Result<Vec<NewRepository>, String> {
    check_repositories_with(drafts, identity_exists, &BTreeMap::new())
}

/// [`check_repositories`] for a list that may name checkouts on this computer: `locals` holds
/// what the service found for each [`RepositoryDraft::local`] (a draft whose entry is missing is
/// refused as not looked at). A local entry's address is its draft's `url` when given, else the
/// checkout's `origin`; its directory defaults to the checkout's folder name.
///
/// # Errors
///
/// The message for the user, as [`check_repositories`], and the reason a checkout can't be used.
pub fn check_repositories_with(
    drafts: &[RepositoryDraft],
    identity_exists: impl Fn(IdentityId) -> bool,
    locals: &BTreeMap<String, Result<LocalSource, String>>,
) -> Result<Vec<NewRepository>, String> {
    if drafts.len() > MAX_REPOSITORIES {
        return Err(format!(
            "a workspace holds at most {MAX_REPOSITORIES} repositories; this list has {}",
            drafts.len()
        ));
    }
    let mut checked: Vec<NewRepository> = Vec::with_capacity(drafts.len());
    for (n, draft) in (1..).zip(drafts) {
        let local = match &draft.local {
            None => None,
            Some(path) => {
                let named =
                    |reason: &str| format!("repository {n} (the checkout {path}): {reason}");
                let found = locals
                    .get(path)
                    .ok_or_else(|| named("it was not looked at"))?
                    .as_ref()
                    .map_err(|e| named(e))?;
                Some(found)
            }
        };
        let typed = draft.url.trim();
        let address = match local {
            Some(found) if typed.is_empty() => found.origin.clone().map_err(|why| {
                format!(
                    "repository {n} (the checkout {}): {why}",
                    found.git_dir.parent().unwrap_or(&found.git_dir).display()
                )
            })?,
            _ => draft.url.clone(),
        };
        let named = |reason: &str| format!("repository {n} ({}): {reason}", shown(&address));
        let url = RepoUrl::parse(&address).map_err(|e| named(&e))?;
        let directory = match (&draft.directory, local) {
            (Some(dir), _) => CheckoutDir::new(dir).map_err(|e| named(e.reason()))?,
            (None, Some(found)) => CheckoutDir::new(&found.folder_name)
                .unwrap_or_else(|_| CheckoutDir::from_url(url.as_str())),
            (None, None) => CheckoutDir::from_url(url.as_str()),
        };
        if let Some(id) = draft.identity
            && !identity_exists(id)
        {
            return Err(named(&format!("there is no identity {}", id.0)));
        }
        let entry = new_entry_id(|id| checked.iter().any(|c| c.repository.entry == *id))
            .map_err(|e| named(&e))?;
        checked.push(NewRepository {
            repository: Repository {
                entry,
                url,
                directory,
            },
            identity: draft.identity,
            pull: draft.pull.unwrap_or(true),
            push: draft.push.unwrap_or(true),
            local: local.map(|found| found.git_dir.clone()),
        });
    }
    for (i, a) in checked.iter().enumerate() {
        for (j, b) in checked.iter().enumerate().skip(i + 1) {
            let (an, bn) = (i + 1, j + 1);
            if a.repository.directory == b.repository.directory {
                let dir = &a.repository.directory;
                return Err(format!(
                    "repositories {an} ({}) and {bn} ({}) both go into {dir}; give one of them \
                     another directory, such as {}",
                    a.repository.url,
                    b.repository.url,
                    free_directory(dir, &checked)
                ));
            }
            if let (Ok(ra), Ok(rb)) = (table_spelling(a), table_spelling(b))
                && ra == rb
            {
                return Err(format!(
                    "repositories {an} ({}) and {bn} ({}) are the same repository, {ra}; list it once",
                    a.repository.url, b.repository.url
                ));
            }
        }
    }
    Ok(checked)
}

/// `url` as an error message may quote it: trimmed, cut to a readable length, and with any user
/// name or password replaced by `***`, so a refusal never echoes a credential.
fn shown(url: &str) -> String {
    const MAX_SHOWN: usize = 200;
    let url = url.trim();
    let shown = match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            match rest.get(..end).and_then(|authority| authority.rfind('@')) {
                Some(at) => format!("{scheme}://***{}", rest.get(at..).unwrap_or_default()),
                None => url.to_owned(),
            }
        }
        None => url.to_owned(),
    };
    if shown.chars().count() > MAX_SHOWN {
        let cut: String = shown.chars().take(MAX_SHOWN).collect();
        format!("{cut}…")
    } else {
        shown
    }
}

/// How the repository table spells an entry's repository, when its address names one.
pub(crate) fn table_spelling(
    repository: &NewRepository,
) -> Result<RepoRef, puddle_store::StoreError> {
    RepoRef::from_https_url(repository.repository.url.as_str())
}

/// `<dir>-2`, `<dir>-3`, ...: the first that no entry uses (cut so it stays a valid name).
fn free_directory(dir: &CheckoutDir, taken: &[NewRepository]) -> CheckoutDir {
    (2..)
        .find_map(|n| {
            let suffix = format!("-{n}");
            let keep = CheckoutDir::MAX_LEN - suffix.len();
            let base: String = dir.as_str().chars().take(keep).collect();
            let candidate = CheckoutDir::new(&format!("{base}{suffix}")).ok()?;
            (!taken.iter().any(|t| t.repository.directory == candidate)).then_some(candidate)
        })
        .unwrap_or_else(|| dir.clone())
}

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
    /// A message for the user that says which rule the path breaks.
    pub fn parse(path: &str) -> Result<Self, String> {
        if path.is_empty() {
            return Err("the start folder must not be empty; use null for the volume root".into());
        }
        if path.len() > Self::MAX_BYTES {
            return Err(format!(
                "the start folder is longer than {} bytes",
                Self::MAX_BYTES
            ));
        }
        if path.starts_with('/') {
            return Err("the start folder is a path under the volume, without a leading /".into());
        }
        if path.chars().any(|c| c.is_control() || c == '\\') {
            return Err("the start folder must not contain control characters or \\".into());
        }
        for part in path.split('/') {
            if matches!(part, "" | "." | "..") {
                return Err(format!(
                    "the start folder has an empty, . or .. part ({path}); name each folder"
                ));
            }
        }
        let first = path.split('/').next().unwrap_or_default();
        if first == ".puddle" || first == CheckoutDir::LOST_AND_FOUND {
            return Err(format!("the start folder must not be inside {first}"));
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

/// Where new workspace ids come from.
pub trait IdSource: Send + Sync {
    /// Draw number `attempt` (counted from 0) for a new workspace called `name`. A create
    /// draws again while the id is taken, at most [`MAX_ID_DRAWS`] times.
    ///
    /// # Errors
    ///
    /// When the source gave nothing, with the reason. That draw counts.
    fn draw(&self, name: &DisplayName, attempt: u32) -> Result<WorkspaceId, String>;
}

/// The operating system's random source, five bytes a draw ([`WorkspaceId::from_random`]);
/// nothing of the name goes into the id. The host uses this one.
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomIds;

impl IdSource for RandomIds {
    fn draw(&self, _name: &DisplayName, _attempt: u32) -> Result<WorkspaceId, String> {
        let mut bytes = [0_u8; 5];
        getrandom::fill(&mut bytes).map_err(|e| format!("no randomness: {e}"))?;
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
    fn draw(&self, name: &DisplayName, attempt: u32) -> Result<WorkspaceId, String> {
        let base = id_for_name(name).map_err(|e| e.to_string())?;
        if attempt == 0 {
            return Ok(base);
        }
        let suffix = format!("-{}", attempt + 1);
        let room = WorkspaceId::MAX_LEN - suffix.len();
        let stem: String = base.as_str().chars().take(room).collect();
        WorkspaceId::new(&format!("{}{suffix}", stem.trim_end_matches('-')))
            .map_err(|e| e.to_string())
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
    /// A request for a workspace with the defaults.
    #[must_use]
    pub fn new(name: DisplayName, repositories: Vec<NewRepository>) -> Self {
        Self {
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

    /// Records whose the new workspace `workspace` is. A create calls it once its id is drawn and
    /// before the workspace is listed, saved or announced anywhere, so nothing about it is ever
    /// seen as another user's. The default records nothing: the workspace is the owner's.
    ///
    /// # Errors
    ///
    /// The record could not be written; the create stops and nothing of it exists.
    fn claim(&self, workspace: &WorkspaceId) -> Result<(), WorkspaceError> {
        let _ = workspace;
        Ok(())
    }
}

/// A [`GitSetup`] that sets nothing up, for services and tests with no store.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGitSetup;

impl GitSetup for NoGitSetup {
    fn set_up<'a>(&'a self, _: &'a WorkspaceId, _: &'a [NewRepository]) -> BoxFuture<'a, ()> {
        Box::pin(async {})
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

/// Opens VS Code on the desktop for a workspace; the desktop shell implements it.
pub trait Launcher: Send + Sync {
    /// Opens an editor window attached to the workspace.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user (VS Code is not installed, the remote
    /// extension is missing, ...).
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>>;

    /// Opens the workspace's browser editor in a window of the desktop shell. The default is for
    /// a launcher with no window (`puddle serve`): it says so, because the editor's password is
    /// handed to a window the shell controls and to nothing else.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user (no window here, the editor gave no session,
    /// the window could not be built).
    fn open_browser<'a>(
        &'a self,
        _workspace: &'a WorkspaceRecord,
        _target: &'a BrowserTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async {
            Err(LaunchError::new(
                "the browser editor opens from the desktop app",
            ))
        })
    }

    /// Closes the workspace's editor windows the way a user would, because its guest is about to
    /// stop or go. Asks and returns: a window that is slow to close is not waited for. The default
    /// has no window.
    fn close_browser<'a>(&'a self, _workspace: &'a WorkspaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Removes what the shell kept for a workspace that is deleted: its windows' browser data and
    /// sign-in. A workspace made later under the same name must start with none of it.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user when something could not be removed (a window
    /// still holds it).
    fn forget_browser<'a>(
        &'a self,
        _workspace: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether the shell keeps browser data under `workspace`'s id (a window's data folder). A
    /// new workspace never gets an id that has some. The default keeps none.
    fn has_browser_data(&self, _workspace: &WorkspaceId) -> bool {
        false
    }
}

/// What a browser editor window needs: where the editor is and how to sign in to it.
#[derive(Clone)]
pub struct BrowserTarget {
    /// The editor's origin on the front, `http://<workspace>.localhost:<port>`.
    pub origin: String,
    /// The front's address, for the sign-in.
    pub front: std::net::SocketAddr,
    /// The workspace's settings, of which a window uses zoom and the clipboard prompt.
    pub settings: puddle_settings::Effective,
    /// How the window signs in to the editor server; `None` when no server is known to run.
    pub login: Option<EditorSignIn>,
    /// What the window opens in the running server, the same place a desktop editor opens; `None`
    /// when the workspace has no running guest to name one, and the window opens the server's own
    /// start page.
    pub open: Option<OpenTarget>,
}

/// A place in a running workspace an editor opens: a checkout, the volume root, or the multi-root
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    /// The absolute path in the guest.
    pub path: String,
    /// Whether `path` is a `.code-workspace` file rather than a folder.
    pub is_workspace_file: bool,
}

/// How a window signs in to a workspace's editor server. Either way the window carries the
/// editor's password (or what it buys) in a cookie set before it navigates, never in a URL.
#[derive(Clone)]
pub enum EditorSignIn {
    /// code-server: the password goes in a login form POST to `/login` on the editor's origin, and
    /// the window gets the session cookie the answer sets.
    Form(Arc<dyn EditorLogin>),
    /// Microsoft's server: the password is its connection token, which the window carries in the
    /// cookie [`CONNECTION_TOKEN_COOKIE`]. The server refuses every request without it, and the
    /// workbench's script reads it from the cookie for its socket's handshake, so it can't be
    /// `HttpOnly`.
    ConnectionToken(Arc<dyn EditorLogin>),
}

/// The cookie Microsoft's server reads its connection token from.
pub const CONNECTION_TOKEN_COOKIE: &str = "vscode-tkn";

impl EditorSignIn {
    /// The way to the password, whichever the use.
    #[must_use]
    pub fn reader(&self) -> &Arc<dyn EditorLogin> {
        match self {
            Self::Form(login) | Self::ConnectionToken(login) => login,
        }
    }
}

impl fmt::Debug for BrowserTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTarget")
            .field("origin", &self.origin)
            .field("front", &self.front)
            .finish_non_exhaustive()
    }
}

/// The editor's password, read when a window signs in. Never log what it returns.
pub trait EditorLogin: Send + Sync {
    /// The password of this boot of the workspace's editor server.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] when the workspace is not running or its agent has none yet.
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>>;
}

/// What a desktop editor opens: a place in a workspace, reached through its ssh host entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopTarget {
    /// The workspace's host name in the ssh config (`puddle-<workspace>`).
    pub ssh_host: String,
    /// The absolute path in the guest to open: a checkout, the volume root, or the multi-root
    /// file.
    pub path: String,
    /// Whether `path` is a `.code-workspace` file rather than a folder.
    pub is_workspace_file: bool,
}

impl DesktopTarget {
    /// The `vscode-remote` URI of the place (`--folder-uri` or `--file-uri` take it), on the
    /// workspace's ssh host; the path is percent-encoded.
    #[must_use]
    pub fn remote_uri(&self) -> String {
        use std::fmt::Write as _;
        let mut path = String::new();
        for byte in self.path.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
                path.push(char::from(byte));
            } else {
                let _ = write!(path, "%{byte:02X}");
            }
        }
        format!("vscode-remote://ssh-remote+{}{path}", self.ssh_host)
    }

    /// The link that opens the place from another program, which VS Code registers as the handler
    /// of `vscode://`: what a browser on the user's own computer follows when the host cannot open
    /// an editor there itself.
    #[must_use]
    pub fn open_uri(&self) -> String {
        let remote = self.remote_uri();
        let rest = remote.strip_prefix("vscode-remote://").unwrap_or(&remote);
        format!("vscode://vscode-remote/{rest}")
    }
}

/// The desktop editor could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LaunchError {
    message: String,
}

impl LaunchError {
    /// An error with this message for the user.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
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
