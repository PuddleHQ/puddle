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
    CheckoutDir, ImageRef, MemoryMib, SandboxName, WorkspaceId, WorkspaceName, WorkspaceStatus,
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
    /// The workspace that runs it; events and settings use this name. It never changes.
    pub name: WorkspaceName,
    /// What the user renamed the workspace to; `None` until they do. Only a label: nothing
    /// else (sandbox, volume, rules, settings) uses it.
    pub display_name: Option<DisplayName>,
    /// The repositories Puddle manages for it, in the order they were added.
    pub repositories: Vec<Repository>,
    /// Where new terminals land and what the editor opens; `None` is the volume root.
    pub start_folder: Option<StartFolder>,
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
}

impl WorkspaceRecord {
    /// A record with the defaults the fake and the tests start from.
    #[must_use]
    pub fn new(id: WorkspaceId, name: WorkspaceName, repositories: Vec<Repository>) -> Self {
        Self {
            id,
            name,
            display_name: None,
            repositories,
            start_folder: None,
            image: DEFAULT_IMAGE.to_owned(),
            memory: MemoryMib::DEFAULT,
            status: WorkspaceStatus::Created,
            busy: None,
            created_at: 0,
            disk_size_mib: DEFAULT_DISK_MIB,
            disk_used_mib: None,
        }
    }
}

impl WorkspaceRecord {
    /// What the user calls the workspace: its display name, else its name.
    #[must_use]
    pub fn label(&self) -> &str {
        self.display_name
            .as_ref()
            .map_or(self.name.as_str(), DisplayName::as_str)
    }
}

/// A name the user chose for a workspace: free text of 1 to [`DisplayName::MAX_CHARS`]
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

/// Whether the workspace `id` may take `display_name` (or go back to its name for `None`) among
/// `records`. The one rule both services apply.
///
/// # Errors
///
/// [`WorkspaceError::NotFound`] for an unknown id; [`WorkspaceError::Conflict`] while the
/// workspace is being created or deleted (there is nothing settled to rename), or when another
/// workspace already goes by that name.
pub fn check_rename<'a>(
    records: impl IntoIterator<Item = &'a WorkspaceRecord> + Clone,
    id: &WorkspaceId,
    display_name: Option<&DisplayName>,
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
    let label = display_name.map_or(record.name.as_str(), DisplayName::as_str);
    match label_taken(records, label, Some(id)) {
        Some(other) => Err(WorkspaceError::Conflict(format!(
            "another workspace is already called {}; choose a different name",
            other.label()
        ))),
        None => Ok(()),
    }
}

/// The image a workspace gets when the request names none.
pub const DEFAULT_IMAGE: &str = "mcr.microsoft.com/devcontainers/base:debian";

/// The volume size a new workspace gets (32 GiB, sparse on the host apart from the 64 MiB journal
/// written when the volume is made).
pub const DEFAULT_DISK_MIB: u64 = 32 * 1024;

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

/// One repository Puddle manages for a workspace: the address it is cloned from
/// and the checkout directly under the volume root it is cloned into. Its identity and its Pull
/// and Push toggles are not part of it: they are the workspace's Git settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// Where it is cloned from.
    pub url: RepoUrl,
    /// The checkout it is cloned into.
    pub directory: CheckoutDir,
}

impl Repository {
    /// The entry for `url` in the directory `git clone` would pick.
    #[must_use]
    pub fn at_default_directory(url: RepoUrl) -> Self {
        let directory = CheckoutDir::from_url(url.as_str());
        Self { url, directory }
    }
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
        checked.push(NewRepository {
            repository: Repository { url, directory },
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
    let record = records
        .into_iter()
        .find(|r| &r.id == id)
        .ok_or_else(|| WorkspaceError::NotFound(format!("no workspace {id}")))?;
    match record.busy {
        Some(busy @ (Operation::Creating | Operation::Deleting)) => {
            Err(WorkspaceError::Conflict(format!(
                "{} is {busy}; change its start folder when that is done",
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

/// What `create` needs. Build it from a request with [`check_repositories`] and the typed names.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewWorkspace {
    /// The workspace's name; its id is derived from it.
    pub name: WorkspaceName,
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
    pub fn new(name: WorkspaceName, repositories: Vec<NewRepository>) -> Self {
        Self {
            name,
            repositories,
            start_folder: None,
            image: None,
            memory: None,
        }
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
        workspace: &'a WorkspaceName,
        repositories: &'a [NewRepository],
    ) -> BoxFuture<'a, ()>;
}

/// A [`GitSetup`] that sets nothing up, for services and tests with no store.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGitSetup;

impl GitSetup for NoGitSetup {
    fn set_up<'a>(&'a self, _: &'a WorkspaceName, _: &'a [NewRepository]) -> BoxFuture<'a, ()> {
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
}

impl Attached {
    /// An editor was opened.
    #[must_use]
    pub fn opened() -> Self {
        Self {
            opened: true,
            message: None,
        }
    }

    /// Nothing was opened, for this reason.
    #[must_use]
    pub fn not_opened(message: impl Into<String>) -> Self {
        Self {
            opened: false,
            message: Some(message.into()),
        }
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

    /// Gives the workspace a display name (`None` goes back to its name). Only the label
    /// changes: the id, the name, the volume and the sandbox stay, so this works on a running
    /// workspace and does not restart anything. [`WorkspaceError::Conflict`] when another
    /// workspace already goes by that name (ignoring case), or while this one is being created
    /// or deleted.
    fn rename<'a>(
        &'a self,
        id: &'a WorkspaceId,
        display_name: Option<DisplayName>,
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

    /// Clones a list entry whose directory is not on the volume again. Accepted
    /// like [`WorkspaceService::add_repository`] with a clone.
    fn clone_again<'a>(
        &'a self,
        _id: &'a WorkspaceId,
        _directory: &'a CheckoutDir,
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
        _directory: &'a CheckoutDir,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        unavailable()
    }

    /// Tells the service that settings changed, so it can apply what takes effect at once (the
    /// SSH endpoint of a running workspace opens or closes with its `direct_ssh` setting).
    /// The default does nothing.
    fn settings_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
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
    fn close_browser<'a>(&'a self, _workspace: &'a WorkspaceName) -> BoxFuture<'a, ()> {
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
        _workspace: &'a WorkspaceName,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Ok(()) })
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
        _: Option<DisplayName>,
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
mod tests {
    use super::*;

    struct SecretLogin(String);

    impl EditorLogin for SecretLogin {
        fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>> {
            Box::pin(async move { Ok(self.0.clone()) })
        }
    }

    struct NoWindow;

    impl Launcher for NoWindow {
        fn open_desktop<'a>(
            &'a self,
            _: &'a WorkspaceRecord,
            _: &'a DesktopTarget,
        ) -> BoxFuture<'a, Result<(), LaunchError>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn a_launcher_with_no_window_has_nothing_to_close_and_nothing_to_forget() {
        let name = WorkspaceName::new("demo").unwrap();
        let record = WorkspaceRecord::new(WorkspaceId::new("demo").unwrap(), name.clone(), vec![]);
        let desktop = DesktopTarget {
            ssh_host: "puddle-demo".into(),
            path: "/".into(),
            is_workspace_file: false,
        };
        futures_util::FutureExt::now_or_never(NoWindow.open_desktop(&record, &desktop))
            .unwrap()
            .unwrap();
        futures_util::FutureExt::now_or_never(NoWindow.close_browser(&name)).unwrap();
        futures_util::FutureExt::now_or_never(NoWindow.forget_browser(&name))
            .unwrap()
            .unwrap();
    }

    #[test]
    fn a_browser_target_shows_where_the_editor_is_and_never_the_way_to_its_password() {
        let secret = "debug-form-secret-value".to_owned();
        let target = BrowserTarget {
            origin: "http://demo.localhost:4100".into(),
            front: "127.0.0.1:4100".parse().unwrap(),
            settings: puddle_settings::resolve(
                &puddle_settings::GlobalSettings::default(),
                None::<&puddle_settings::WorkspaceSettings>,
            ),
            login: Some(EditorSignIn::Form(Arc::new(SecretLogin(secret.clone())))),
        };
        let login = target.login.as_ref().unwrap().reader();
        assert_eq!(
            futures_util::FutureExt::now_or_never(login.password())
                .unwrap()
                .unwrap(),
            secret
        );
        let shown = format!("{target:?}");
        assert!(shown.contains("http://demo.localhost:4100"), "{shown}");
        assert!(shown.contains("127.0.0.1:4100"), "{shown}");
        assert!(!shown.contains(&secret), "{shown}");
        assert!(!shown.contains("SecretLogin"), "{shown}");
    }

    #[test]
    fn https_urls_pass() {
        for ok in [
            "https://github.com/acme/api",
            "https://github.com/acme/api.git",
            "HTTPS://GitHub.com/acme/api",
            "https://dev.azure.com/org/proj/_git/repo",
            "https://git.example.com:8443/a/b",
            "  https://github.com/acme/api  ",
        ] {
            assert_eq!(RepoUrl::parse(ok).unwrap().as_str(), ok.trim());
        }
    }

    #[test]
    fn ssh_remotes_get_the_not_supported_message() {
        for ssh in [
            "git@github.com:acme/api.git",
            "ssh://git@github.com/acme/api.git",
            "SSH://github.com/acme/api",
            "github.com:acme/api",
            "user@host:path",
        ] {
            let err = RepoUrl::parse(ssh).unwrap_err();
            assert!(
                err.contains("SSH remotes are not supported yet"),
                "{ssh}: {err}"
            );
            assert!(err.contains("HTTPS"), "{err}");
        }
    }

    #[test]
    fn other_urls_say_what_to_change() {
        let cases = [
            ("http://github.com/a/b", "https://"),
            ("git://github.com/a/b", "https://"),
            ("/home/me/repo", "https://"),
            ("C:\\repos\\api", "https://"),
            ("file:///repo", "https://"),
            ("", "https://"),
            ("https://", "no host"),
            ("https:///path", "no host"),
            ("https://:443/x", "no host"),
            (
                "https://user:secret@github.com/a/b",
                "user name and password",
            ),
            ("https://token@github.com/a/b", "user name and password"),
            ("https://github.com/a b", "spaces"),
            ("https://github.com/a\nb", "control"),
        ];
        for (url, wants) in cases {
            let err = RepoUrl::parse(url).unwrap_err();
            assert!(err.contains(wants), "{url:?}: {err}");
            assert!(!err.contains("secret"), "{err}");
        }
        let long = format!("https://github.com/{}", "a".repeat(MAX_REPO_URL_LEN));
        assert!(RepoUrl::parse(&long).unwrap_err().contains("longer than"));
    }

    #[test]
    fn a_check_is_clean_only_without_findings_or_errors() {
        let id = WorkspaceId::new("w").unwrap();
        let mut check = DeleteCheck {
            workspace: id,
            repos: vec![RepoFindings::default()],
            other: Listing::default(),
            errors: vec![],
            removes_sandbox: None,
            volume_missing: false,
            fingerprint: String::new(),
        };
        assert!(check.is_clean());
        check.repos[0].stashes.more = 1;
        assert!(!check.is_clean());
        check.repos[0].stashes.more = 0;
        check.other.items.push("notes.txt".into());
        assert!(!check.is_clean());
        check.other = Listing::default();
        check.errors.push("repo: unreadable".into());
        assert!(!check.is_clean());
    }

    #[test]
    fn a_missing_volume_changes_the_fingerprint() {
        let id = WorkspaceId::new("w").unwrap();
        let plain = DeleteCheck::new(id, vec![], Listing::default(), vec![], None);
        let gone = plain.clone().with_volume_missing();
        assert!(gone.volume_missing && gone.is_clean());
        assert_ne!(plain.fingerprint, gone.fingerprint);
    }

    #[tokio::test]
    async fn the_empty_service_says_it_is_unavailable() {
        let svc = NoWorkspaces;
        let id = WorkspaceId::new("w").unwrap();
        let name = WorkspaceName::new("w").unwrap();
        let dir = CheckoutDir::new("a").unwrap();
        let url = RepoUrl::parse("https://example.com/a").unwrap();
        let repos = vec![NewRepository {
            repository: Repository::at_default_directory(url),
            identity: None,
            pull: true,
            push: true,
            local: None,
        }];
        let all = [
            svc.list().await.map(|_| ()),
            svc.get(&id).await.map(|_| ()),
            svc.create(NewWorkspace::new(name, repos), &NoGitSetup)
                .await
                .map(|_| ()),
            svc.start(&id).await.map(|_| ()),
            svc.stop(&id).await.map(|_| ()),
            svc.reclaim(&id).await.map(|_| ()),
            svc.delete_check(&id).await.map(|_| ()),
            svc.delete(&id, None).await.map(|_| ()),
            svc.rename(&id, None).await.map(|_| ()),
            svc.attach(&id, AttachMode::Browser).await.map(|_| ()),
            svc.checkouts(&id).await.map(|_| ()),
            svc.add_repository(
                &id,
                AddRepository::Existing {
                    directory: dir.clone(),
                    seen: RepoUrl::parse("https://github.com/o/r").unwrap(),
                    identity: None,
                    pull: true,
                    push: true,
                },
                &NoGitSetup,
            )
            .await
            .map(|_| ()),
            svc.clone_again(&id, &dir, &NoGitSetup).await.map(|_| ()),
            svc.remove_repository(&id, &dir).await.map(|_| ()),
        ];
        for result in all {
            assert!(matches!(result, Err(WorkspaceError::Unavailable(_))));
        }
    }

    #[tokio::test]
    async fn a_service_with_no_credential_store_keeps_no_logins_to_list_or_forget() {
        let id = WorkspaceId::new("w").unwrap();
        let listed = NoWorkspaces.kept_logins(&id).await;
        assert!(
            matches!(listed, Err(WorkspaceError::Unavailable(ref why)) if why.contains("no logins")),
            "{listed:?}"
        );
        let forgotten = NoWorkspaces.forget_login(&id, "claude").await;
        assert!(
            matches!(forgotten, Err(WorkspaceError::Unavailable(ref why)) if why.contains("no logins")),
            "{forgotten:?}"
        );
    }

    #[tokio::test]
    async fn a_service_with_no_guest_does_not_check_shell_init() {
        let id = WorkspaceId::new("w").unwrap();
        let check = NoWorkspaces.check_shell_init(&id, "echo hi\n").await;
        assert!(
            matches!(check, Ok(ShellInitCheck::NotRun(ref why)) if why.contains("no workspace")),
            "{check:?}"
        );
    }

    #[test]
    fn display_names_are_trimmed_and_bounded() {
        assert_eq!(
            DisplayName::parse("  Web shop ").unwrap().as_str(),
            "Web shop"
        );
        assert_eq!(
            DisplayName::parse("Café 日本").unwrap().as_str(),
            "Café 日本"
        );
        assert!(DisplayName::parse("a".repeat(60).as_str()).is_ok());
        for (bad, wants) in [
            ("", "empty"),
            ("   ", "empty"),
            (&"a".repeat(61), "longer than 60"),
            ("two\nlines", "line breaks"),
            ("tab\there", "control"),
        ] {
            let err = DisplayName::parse(bad).unwrap_err();
            assert!(err.contains(wants), "{bad:?}: {err}");
        }
        // Sixty multi-byte characters are within the limit: it counts characters, not bytes.
        assert!(DisplayName::parse(&"é".repeat(60)).is_ok());
    }

    #[test]
    fn a_label_is_taken_ignoring_case_and_the_workspace_itself() {
        let mk = |id: &str, shown: Option<&str>| {
            let mut r = WorkspaceRecord::new(
                WorkspaceId::new(id).unwrap(),
                WorkspaceName::new(id).unwrap(),
                Vec::new(),
            );
            r.display_name = shown.map(|s| DisplayName::parse(s).unwrap());
            r
        };
        let all = [mk("alpha", None), mk("beta", Some("Web Shop"))];
        assert_eq!(all[1].label(), "Web Shop");
        assert_eq!(all[0].label(), "alpha");
        let beta = WorkspaceId::new("beta").unwrap();
        assert_eq!(label_taken(&all, "web shop", None).unwrap().id, beta);
        assert_eq!(
            label_taken(&all, "ALPHA", None).unwrap().name.as_str(),
            "alpha"
        );
        assert!(label_taken(&all, "web shop", Some(&beta)).is_none());
        // A workspace that was renamed no longer goes by its old name.
        assert!(label_taken(&all, "beta", None).is_none());
    }

    #[test]
    fn a_rename_is_refused_while_creating_or_deleting_and_on_a_clash() {
        let mk = |id: &str| {
            WorkspaceRecord::new(
                WorkspaceId::new(id).unwrap(),
                WorkspaceName::new(id).unwrap(),
                Vec::new(),
            )
        };
        let (mut a, b) = (mk("alpha"), mk("beta"));
        let name = |s: &str| Some(DisplayName::parse(s).unwrap());
        let ida = a.id.clone();
        let check = |a: &WorkspaceRecord, to: Option<&DisplayName>| check_rename([a, &b], &ida, to);
        assert!(check(&a, name("Web").as_ref()).is_ok());
        assert!(
            check(&a, name("alpha").as_ref()).is_ok(),
            "its own name is not a clash"
        );
        assert!(matches!(
            check(&a, name("BETA").as_ref()),
            Err(WorkspaceError::Conflict(m)) if m.contains("already called beta")
        ));
        a.busy = Some(Operation::Creating);
        assert!(
            matches!(check(&a, None), Err(WorkspaceError::Conflict(m)) if m.contains("creating"))
        );
        a.busy = Some(Operation::Deleting);
        assert!(matches!(check(&a, None), Err(WorkspaceError::Conflict(_))));
        for busy in [
            Operation::Starting,
            Operation::Stopping,
            Operation::Reclaiming,
        ] {
            a.busy = Some(busy);
            assert!(check(&a, name("Web").as_ref()).is_ok(), "{busy}");
        }
        let missing = WorkspaceId::new("nope").unwrap();
        assert!(matches!(
            check_rename([&a, &b], &missing, None),
            Err(WorkspaceError::NotFound(_))
        ));
    }

    fn draft(url: &str) -> RepositoryDraft {
        RepositoryDraft {
            url: url.to_owned(),
            ..RepositoryDraft::default()
        }
    }

    #[test]
    fn a_checkout_on_this_computer_takes_its_origin_and_folder_name_unless_given() {
        let source = |folder: &str, origin: Result<&str, &str>| LocalSource {
            git_dir: PathBuf::from(format!("/home/u/{folder}/.git")),
            folder_name: folder.into(),
            origin: origin.map(str::to_owned).map_err(str::to_owned),
        };
        let local = |path: &str, url: &str| RepositoryDraft {
            url: url.to_owned(),
            local: Some(path.to_owned()),
            ..RepositoryDraft::default()
        };
        let mut locals = BTreeMap::new();
        locals.insert(
            "/home/u/Shop".to_owned(),
            Ok(source("Shop", Ok("https://github.com/acme/shop.git"))),
        );
        locals.insert(
            "/home/u/none".to_owned(),
            Ok(source("none", Err("it has no origin remote"))),
        );
        locals.insert(
            "/home/u/plain".to_owned(),
            Err("/home/u/plain is not a git checkout: it has no .git".to_owned()),
        );
        let checked = check_repositories_with(
            &[
                local("/home/u/Shop", ""),
                local("/home/u/none", "https://example.org/acme/x.git"),
            ],
            |_| false,
            &locals,
        )
        .unwrap();
        assert_eq!(
            checked[0].repository.url.as_str(),
            "https://github.com/acme/shop.git"
        );
        assert_eq!(checked[0].repository.directory.as_str(), "Shop");
        assert_eq!(checked[0].local, Some(PathBuf::from("/home/u/Shop/.git")));
        // A given address stands in for a missing origin.
        assert_eq!(
            checked[1].repository.url.as_str(),
            "https://example.org/acme/x.git"
        );
        assert!(checked.iter().all(|r| r.pull && r.push));
        let refused =
            |d: RepositoryDraft| check_repositories_with(&[d], |_| false, &locals).unwrap_err();
        assert_eq!(
            refused(local("/home/u/none", "")),
            "repository 1 (the checkout /home/u/none): it has no origin remote"
        );
        assert_eq!(
            refused(local("/home/u/plain", "")),
            "repository 1 (the checkout /home/u/plain): /home/u/plain is not a git checkout: it has no .git"
        );
        assert_eq!(
            refused(local("/home/u/unseen", "")),
            "repository 1 (the checkout /home/u/unseen): it was not looked at"
        );
        // The address still passes the create's checks.
        assert!(refused(local("/home/u/Shop", "http://example.org/x")).contains("https://"));
        // Without a draft that names one, the plain check needs no service.
        assert_eq!(
            check_repositories(&[draft("https://github.com/a/b")], |_| false).unwrap()[0].local,
            None
        );
    }

    #[test]
    fn the_default_service_has_no_checkouts_on_this_computer() {
        let err = NoWorkspaces.local_checkout("/home/u/shop").unwrap_err();
        assert!(err.contains("not available"), "{err}");
    }

    #[test]
    fn a_repository_list_takes_defaults_and_keeps_its_order() {
        let checked = check_repositories(
            &[
                draft("https://github.com/acme/api.git"),
                RepositoryDraft {
                    directory: Some("site".into()),
                    identity: Some(IdentityId(4)),
                    pull: Some(false),
                    ..draft("https://github.com/acme/web")
                },
            ],
            |id| id == IdentityId(4),
        )
        .unwrap();
        let shape: Vec<_> = checked
            .iter()
            .map(|r| {
                (
                    r.repository.url.as_str(),
                    r.repository.directory.as_str(),
                    r.identity,
                    r.pull,
                    r.push,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("https://github.com/acme/api.git", "api", None, true, true),
                (
                    "https://github.com/acme/web",
                    "site",
                    Some(IdentityId(4)),
                    false,
                    true
                ),
            ]
        );
        assert_eq!(check_repositories(&[], |_| false).unwrap(), []);
    }

    #[test]
    fn a_repository_list_is_refused_naming_the_entry() {
        let refused =
            |drafts: &[RepositoryDraft]| check_repositories(drafts, |_| false).unwrap_err();
        let clash = refused(&[
            draft("https://github.com/acme/api"),
            draft("https://github.com/acme/web"),
            draft("https://gitlab.com/other/api.git"),
        ]);
        assert_eq!(
            clash,
            "repositories 1 (https://github.com/acme/api) and 3 (https://gitlab.com/other/api.git) \
             both go into api; give one of them another directory, such as api-2"
        );
        let taken = refused(&[
            draft("https://github.com/acme/api"),
            RepositoryDraft {
                directory: Some("api-2".into()),
                ..draft("https://github.com/acme/web")
            },
            draft("https://gitlab.com/other/api.git"),
        ]);
        assert!(taken.ends_with("such as api-3"), "{taken}");
        let twice = refused(&[
            draft("https://github.com/Acme/API.git"),
            RepositoryDraft {
                directory: Some("again".into()),
                ..draft("https://github.com/acme/api/")
            },
        ]);
        assert_eq!(
            twice,
            "repositories 1 (https://github.com/Acme/API.git) and 2 (https://github.com/acme/api/) \
             are the same repository, github.com/acme/api; list it once"
        );
        let ghost = refused(&[
            draft("https://github.com/acme/api"),
            RepositoryDraft {
                identity: Some(IdentityId(9)),
                ..draft("https://github.com/acme/web")
            },
        ]);
        assert_eq!(
            ghost,
            "repository 2 (https://github.com/acme/web): there is no identity 9"
        );
        let ssh = refused(&[draft("git@github.com:acme/api.git")]);
        assert!(
            ssh.starts_with("repository 1 (git@github.com:acme/api.git): SSH remotes"),
            "{ssh}"
        );
        let bad_dir = refused(&[RepositoryDraft {
            directory: Some("../up".into()),
            ..draft("https://github.com/acme/api")
        }]);
        assert!(bad_dir.contains("must not start with"), "{bad_dir}");
        let many: Vec<_> = (0..=MAX_REPOSITORIES)
            .map(|n| draft(&format!("https://github.com/acme/r{n}")))
            .collect();
        assert_eq!(
            refused(&many),
            "a workspace holds at most 100 repositories; this list has 101"
        );
        assert!(check_repositories(&many[..MAX_REPOSITORIES], |_| false).is_ok());
    }

    #[test]
    fn a_refused_address_is_named_without_its_credentials() {
        let err = check_repositories(&[draft("  https://me:tok@github.com/a/b  ")], |_| false)
            .unwrap_err();
        assert!(
            err.starts_with("repository 1 (https://***@github.com/a/b): remove the user name"),
            "{err}"
        );
        assert!(!err.contains("tok"), "{err}");
        let long = format!("https://github.com/{}", "a".repeat(3000));
        let err = check_repositories(&[draft(&long)], |_| false).unwrap_err();
        assert!(err.len() < 400, "{}", err.len());
    }

    #[test]
    fn a_long_directory_still_gets_a_free_name_that_fits() {
        let long = "a".repeat(CheckoutDir::MAX_LEN);
        let refused = check_repositories(
            &[
                RepositoryDraft {
                    directory: Some(long.clone()),
                    ..draft("https://github.com/acme/one")
                },
                RepositoryDraft {
                    directory: Some(long.clone()),
                    ..draft("https://github.com/acme/two")
                },
            ],
            |_| false,
        )
        .unwrap_err();
        let suggested = refused.rsplit(' ').next().unwrap();
        assert_eq!(suggested, format!("{}-2", "a".repeat(98)));
    }

    #[test]
    fn a_start_folder_is_a_plain_path_under_the_volume() {
        for ok in ["api", "api/src", "web site/app", "a.b/c_d"] {
            assert_eq!(StartFolder::parse(ok).unwrap().as_str(), ok);
        }
        assert!(StartFolder::parse(&"a".repeat(255)).is_ok());
        for (bad, why) in [
            ("", "empty"),
            ("/api", "leading /"),
            ("api/", "empty, . or .."),
            ("a//b", "empty, . or .."),
            ("./api", "empty, . or .."),
            ("api/../..", "empty, . or .."),
            ("..", "empty, . or .."),
            ("a\\b", "control characters"),
            ("a\nb", "control characters"),
            (".puddle", "inside .puddle"),
            (".puddle/editor", "inside .puddle"),
            ("lost+found/x", "inside lost+found"),
            (&"a".repeat(256), "255 bytes"),
        ] {
            let err = StartFolder::parse(bad).unwrap_err();
            assert!(err.contains(why), "{bad:?}: {err}");
        }
        assert_eq!(
            StartFolder::of_checkout(&CheckoutDir::new("src").unwrap()).as_str(),
            "src"
        );
    }

    #[test]
    fn operations_have_wire_names() {
        assert_eq!(Operation::Reclaiming.to_string(), "reclaiming");
        assert_eq!(Operation::Deleting.to_string(), "deleting");
    }
}
