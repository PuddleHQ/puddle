// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's repositories: URLs, entries and the checks on a requested set.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use puddle_store::{IdentityId, RepoRef};
use puddle_types::{CheckoutDir, EntryId};

use super::MAX_ID_DRAWS;

/// Why a repository URL is refused. The messages are for the user and say what to change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RepoUrlError {
    /// An `ssh://` or scp-like remote.
    #[error("ssh remotes are not supported yet; use the repository's HTTPS URL instead")]
    Ssh,
    /// Longer than [`MAX_REPO_URL_LEN`].
    #[error("the repository URL is longer than {MAX_REPO_URL_LEN} characters")]
    TooLong,
    /// A space or control character.
    #[error("the repository URL must not contain spaces or control characters")]
    Whitespace,
    /// Not `https://`.
    #[error("use an https:// URL for the repository")]
    NotHttps,
    /// A user name or password in the URL.
    #[error("remove the user name and password from the URL; Puddle supplies credentials itself")]
    Credentials,
    /// No host.
    #[error("the repository URL has no host")]
    NoHost,
}

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
    /// [`RepoUrlError`]: SSH remotes get the "not supported yet" one, anything else that is not a
    /// plain `https://` URL says what to change.
    pub fn parse(url: &str) -> Result<Self, RepoUrlError> {
        let url = url.trim();
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("ssh://") || is_scp_like(url) {
            return Err(RepoUrlError::Ssh);
        }
        if url.len() > MAX_REPO_URL_LEN {
            return Err(RepoUrlError::TooLong);
        }
        if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(RepoUrlError::Whitespace);
        }
        let Some(rest) = lower.strip_prefix("https://") else {
            return Err(RepoUrlError::NotHttps);
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.contains('@') {
            return Err(RepoUrlError::Credentials);
        }
        let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
        if host.is_empty() {
            return Err(RepoUrlError::NoHost);
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

/// Why a repository entry got no id: every draw was taken or gave nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("no free repository entry id was found; the system's random source may be broken")]
#[non_exhaustive]
pub struct NoFreeEntryId;

/// A fresh entry id ([`EntryId::from_random`] over the operating system's random source) that
/// `taken` does not know, drawn up to [`MAX_ID_DRAWS`] times; `taken` says whether the entry
/// list of the workspace already has it.
///
/// # Errors
///
/// [`NoFreeEntryId`] when every draw was taken or the random source gave nothing.
pub fn new_entry_id(taken: impl Fn(&EntryId) -> bool) -> Result<EntryId, NoFreeEntryId> {
    draw_entry_id(|bytes| getrandom::fill(bytes), taken)
}

/// [`new_entry_id`] over any source of five bytes.
pub(super) fn draw_entry_id<E: fmt::Display>(
    mut fill: impl FnMut(&mut [u8; 5]) -> Result<(), E>,
    taken: impl Fn(&EntryId) -> bool,
) -> Result<EntryId, NoFreeEntryId> {
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
    Err(NoFreeEntryId)
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

/// Why a folder on this computer can't be the start of a workspace. The messages are for the
/// user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LocalCheckoutError {
    /// The service does not look at folders.
    #[error("starting from a checkout on this computer is not available here")]
    NotAvailable,
    /// The folder has no `.git`.
    #[error("{} is not a git checkout: it has no .git", folder.display())]
    NotCheckout {
        /// The folder as looked at.
        folder: PathBuf,
    },
    /// The checkout reader or the runtime refused the folder, with its own reason.
    #[error("{0}")]
    Refused(String),
}

/// A checkout on this computer, as the service found it (see
/// [`super::WorkspaceService::local_checkout`]).
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

/// Why a checkout on this computer can't stand for a repository of a create.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CheckoutProblem {
    /// The service found nothing for the entry.
    #[error("it was not looked at")]
    NotLooked,
    /// The folder can't be used.
    #[error(transparent)]
    Unusable(LocalCheckoutError),
    /// The checkout has no `origin` Puddle can use, with the reason.
    #[error("{0}")]
    NoOrigin(String),
}

/// What is wrong with one entry of a create's repository list.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EntryProblem {
    /// The address is refused.
    #[error(transparent)]
    Url(RepoUrlError),
    /// The directory is not a checkout directory.
    #[error("{}", .0.reason())]
    Directory(puddle_types::ValidationError),
    /// The picked identity does not exist.
    #[error("there is no identity {}", .0.0)]
    UnknownIdentity(IdentityId),
    /// No entry id could be drawn.
    #[error(transparent)]
    NoEntryId(NoFreeEntryId),
}

/// Why a create's repository list is refused. The messages are for the user and name the entry by
/// its place in the list and its address.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RepositoryListError {
    /// More than [`MAX_REPOSITORIES`].
    #[error("a workspace holds at most {MAX_REPOSITORIES} repositories; this list has {count}")]
    TooMany {
        /// How many the list has.
        count: usize,
    },
    /// An entry that starts from a checkout on this computer can't use it.
    #[error("repository {n} (the checkout {checkout}): {problem}")]
    Checkout {
        /// The entry's place in the list, from 1.
        n: usize,
        /// The checkout as given or as found.
        checkout: String,
        /// What is wrong.
        problem: CheckoutProblem,
    },
    /// An entry is refused.
    #[error("repository {n} ({address}): {problem}")]
    Entry {
        /// The entry's place in the list, from 1.
        n: usize,
        /// The address, safe to quote.
        address: String,
        /// What is wrong.
        problem: EntryProblem,
    },
    /// Two entries go into one directory.
    #[error(
        "repositories {first} ({first_url}) and {second} ({second_url}) both go into {directory}; \
         give one of them another directory, such as {suggestion}"
    )]
    SameDirectory {
        /// The first entry's place, from 1.
        first: usize,
        /// Its address.
        first_url: String,
        /// The second entry's place, from 1.
        second: usize,
        /// Its address.
        second_url: String,
        /// The directory both use.
        directory: CheckoutDir,
        /// A free directory to use instead.
        suggestion: CheckoutDir,
    },
    /// Two entries name one repository.
    #[error(
        "repositories {first} ({first_url}) and {second} ({second_url}) are the same repository, \
         {spelling}; list it once"
    )]
    SameRepository {
        /// The first entry's place, from 1.
        first: usize,
        /// Its address.
        first_url: String,
        /// The second entry's place, from 1.
        second: usize,
        /// Its address.
        second_url: String,
        /// How the repository table spells it.
        spelling: String,
    },
}

/// Checks a create's repository list as a whole, before anything exists. Each
/// refusal names the entry by its place in the list and its address: an address the create
/// checks refuse, a bad directory, two entries with one directory (with a free name to use
/// instead), the same repository twice in the repository table's spelling, an identity
/// `identity_exists` does not know, or more than [`MAX_REPOSITORIES`].
///
/// # Errors
///
/// A [`RepositoryListError`], whose message is for the user.
pub fn check_repositories(
    drafts: &[RepositoryDraft],
    identity_exists: impl Fn(IdentityId) -> bool,
) -> Result<Vec<NewRepository>, RepositoryListError> {
    check_repositories_with(drafts, identity_exists, &BTreeMap::new())
}

/// [`check_repositories`] for a list that may name checkouts on this computer: `locals` holds
/// what the service found for each [`RepositoryDraft::local`] (a draft whose entry is missing is
/// refused as not looked at). A local entry's address is its draft's `url` when given, else the
/// checkout's `origin`; its directory defaults to the checkout's folder name.
///
/// # Errors
///
/// A [`RepositoryListError`], as [`check_repositories`], and the reason a checkout can't be used.
pub fn check_repositories_with(
    drafts: &[RepositoryDraft],
    identity_exists: impl Fn(IdentityId) -> bool,
    locals: &BTreeMap<String, Result<LocalSource, LocalCheckoutError>>,
) -> Result<Vec<NewRepository>, RepositoryListError> {
    if drafts.len() > MAX_REPOSITORIES {
        return Err(RepositoryListError::TooMany {
            count: drafts.len(),
        });
    }
    let mut checked: Vec<NewRepository> = Vec::with_capacity(drafts.len());
    for (n, draft) in (1..).zip(drafts) {
        let local = match &draft.local {
            None => None,
            Some(path) => {
                let named = |problem: CheckoutProblem| RepositoryListError::Checkout {
                    n,
                    checkout: path.clone(),
                    problem,
                };
                let found = locals
                    .get(path)
                    .ok_or_else(|| named(CheckoutProblem::NotLooked))?
                    .as_ref()
                    .map_err(|e| named(CheckoutProblem::Unusable(e.clone())))?;
                Some(found)
            }
        };
        let typed = draft.url.trim();
        let address = match local {
            Some(found) if typed.is_empty() => {
                found
                    .origin
                    .clone()
                    .map_err(|why| RepositoryListError::Checkout {
                        n,
                        checkout: found
                            .git_dir
                            .parent()
                            .unwrap_or(&found.git_dir)
                            .display()
                            .to_string(),
                        problem: CheckoutProblem::NoOrigin(why),
                    })?
            }
            _ => draft.url.clone(),
        };
        let named = |problem: EntryProblem| RepositoryListError::Entry {
            n,
            address: shown(&address),
            problem,
        };
        let url = RepoUrl::parse(&address).map_err(|e| named(EntryProblem::Url(e)))?;
        let directory = match (&draft.directory, local) {
            (Some(dir), _) => {
                CheckoutDir::new(dir).map_err(|e| named(EntryProblem::Directory(e)))?
            }
            (None, Some(found)) => CheckoutDir::new(&found.folder_name)
                .unwrap_or_else(|_| CheckoutDir::from_url(url.as_str())),
            (None, None) => CheckoutDir::from_url(url.as_str()),
        };
        if let Some(id) = draft.identity
            && !identity_exists(id)
        {
            return Err(named(EntryProblem::UnknownIdentity(id)));
        }
        let entry = new_entry_id(|id| checked.iter().any(|c| c.repository.entry == *id))
            .map_err(|e| named(EntryProblem::NoEntryId(e)))?;
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
    refuse_duplicates(&checked)?;
    Ok(checked)
}

/// Refuses two entries that go into one directory or name one repository.
fn refuse_duplicates(checked: &[NewRepository]) -> Result<(), RepositoryListError> {
    for (i, a) in checked.iter().enumerate() {
        for (j, b) in checked.iter().enumerate().skip(i + 1) {
            let (an, bn) = (i + 1, j + 1);
            if a.repository.directory == b.repository.directory {
                let dir = &a.repository.directory;
                return Err(RepositoryListError::SameDirectory {
                    first: an,
                    first_url: a.repository.url.to_string(),
                    second: bn,
                    second_url: b.repository.url.to_string(),
                    directory: dir.clone(),
                    suggestion: free_directory(dir, checked),
                });
            }
            if let (Ok(ra), Ok(rb)) = (table_spelling(a), table_spelling(b))
                && ra == rb
            {
                return Err(RepositoryListError::SameRepository {
                    first: an,
                    first_url: a.repository.url.to_string(),
                    second: bn,
                    second_url: b.repository.url.to_string(),
                    spelling: ra.to_string(),
                });
            }
        }
    }
    Ok(())
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
