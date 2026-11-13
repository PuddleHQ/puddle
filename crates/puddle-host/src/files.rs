// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's small state files: the settings documents and the workspace book.
//!
//! Both are JSON written atomically and owner-only ([`puddle_fs::private::write_atomic`]). A
//! missing file is a fresh install; one that exists but cannot be read or parsed is an error, so
//! a damaged file is never mistaken for an empty one.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use puddle_api::{RepoUrl, Repository, SettingsRepo, SettingsRepoError, StartFolder};
use puddle_types::{CheckoutDir, EntryId, SessionUser, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::HostError;

/// Why a state file could not be read or written. The messages name the file.
#[derive(Debug, thiserror::Error)]
enum StateFileError {
    /// The file holds something that is not JSON.
    #[error("{} is not valid JSON: {source}", path.display())]
    NotJson {
        /// The file.
        path: PathBuf,
        /// What the parser said.
        source: serde_json::Error,
    },
    /// The file exists and could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// The document could not be encoded.
    #[error(transparent)]
    Encode(serde_json::Error),
    /// The file could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Write {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
}

/// Reads `path` as JSON, `None` when it does not exist.
fn read_json(path: &Path) -> Result<Option<Value>, StateFileError> {
    match std::fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| StateFileError::NotJson {
                    path: path.to_owned(),
                    source,
                })
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StateFileError::Read {
            path: path.to_owned(),
            source,
        }),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), StateFileError> {
    let body = serde_json::to_vec_pretty(value).map_err(StateFileError::Encode)?;
    puddle_fs::private::write_atomic(path, &body).map_err(|source| StateFileError::Write {
        path: path.to_owned(),
        source,
    })
}

/// An entry id the entry's repository list gives twice.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct DuplicateEntry(EntryId);

/// A damaged entry of workspace `id`: `what` was being read and `source` is wrong with it.
fn bad_entry<E: std::error::Error + Send + Sync + 'static>(
    id: &str,
    what: &'static str,
    source: E,
) -> BookError {
    BookError::Entry {
        id: id.to_owned(),
        what,
        source: Box::new(source),
    }
}

/// Why a workspace book is refused. The messages name the file or the entry.
#[derive(Debug, thiserror::Error)]
enum BookError {
    /// The file could not be read or written.
    #[error(transparent)]
    File(#[from] StateFileError),
    /// The file is JSON but not a workspace list.
    #[error("{} is not a workspace list: {source}", path.display())]
    NotAList {
        /// The file.
        path: PathBuf,
        /// What the parser said.
        source: serde_json::Error,
    },
    /// A newer Puddle wrote the file.
    #[error("{} was written by a newer Puddle (version {version})", path.display())]
    Newer {
        /// The file.
        path: PathBuf,
        /// Its version.
        version: u32,
    },
    /// A version no Puddle wrote.
    #[error("{} has version {version}, which no Puddle wrote", path.display())]
    Unknown {
        /// The file.
        path: PathBuf,
        /// Its version.
        version: u32,
    },
    /// A retired id is not an id.
    #[error("retired id {id:?} is not an id: {source}")]
    RetiredId {
        /// The id as stored.
        id: String,
        /// What is wrong with it.
        source: puddle_types::ValidationError,
    },
    /// One workspace's entry is damaged.
    #[error("workspace {id:?} {what}: {source}")]
    Entry {
        /// The id as stored.
        id: String,
        /// What was being read.
        what: &'static str,
        /// What is wrong with it.
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Settings documents as files: `global.json` and `workspaces/<name>.json` in one folder.
///
/// The API versions and validates the documents; this only keeps them. Workspace names are
/// validated DNS labels, so a name is always a safe file name. Reads are served from memory after
/// the first one (the proxy asks for a workspace's settings on every new connection); saves write
/// the file first and the memory second, and this process is the file's only writer.
///
/// Workspace documents used to live in `sandboxes/<name>.json`. A document with no file in
/// `workspaces/` is read from there, and saving it moves it to `workspaces/`.
#[derive(Debug)]
pub struct FileSettings {
    dir: PathBuf,
    cache: Mutex<Cache>,
}

#[derive(Debug, Default)]
struct Cache {
    global: Option<Value>,
    global_read: bool,
    workspaces: BTreeMap<WorkspaceId, Option<Value>>,
    app_icon: Option<Vec<u8>>,
    app_icon_read: bool,
}

impl FileSettings {
    /// Settings kept in `dir` (created when first written).
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            cache: Mutex::new(Cache::default()),
        }
    }

    fn global_path(&self) -> PathBuf {
        self.dir.join("global.json")
    }

    fn app_icon_path(&self) -> PathBuf {
        self.dir.join("app-icon.png")
    }

    fn workspace_path(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir
            .join("workspaces")
            .join(format!("{workspace}.json"))
    }

    /// Where workspace documents were kept before they were called workspaces.
    fn legacy_path(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join("sandboxes").join(format!("{workspace}.json"))
    }

    fn cache(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl SettingsRepo for FileSettings {
    fn load_global(&self) -> Result<Option<Value>, SettingsRepoError> {
        let mut cache = self.cache();
        if cache.global_read {
            return Ok(cache.global.clone());
        }
        let loaded =
            read_json(&self.global_path()).map_err(|e| SettingsRepoError::new(e.to_string()))?;
        cache.global.clone_from(&loaded);
        cache.global_read = true;
        Ok(loaded)
    }

    fn save_global(&self, document: Value) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        write_json(&self.global_path(), &document)
            .map_err(|e| SettingsRepoError::new(e.to_string()))?;
        cache.global = Some(document);
        cache.global_read = true;
        Ok(())
    }

    fn load_workspace(&self, workspace: &WorkspaceId) -> Result<Option<Value>, SettingsRepoError> {
        let mut cache = self.cache();
        if let Some(cached) = cache.workspaces.get(workspace) {
            return Ok(cached.clone());
        }
        let mut loaded = read_json(&self.workspace_path(workspace))
            .map_err(|e| SettingsRepoError::new(e.to_string()))?;
        if loaded.is_none() {
            loaded = read_json(&self.legacy_path(workspace))
                .map_err(|e| SettingsRepoError::new(e.to_string()))?;
        }
        cache.workspaces.insert(workspace.clone(), loaded.clone());
        Ok(loaded)
    }

    fn save_workspace(
        &self,
        workspace: &WorkspaceId,
        document: Value,
    ) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        write_json(&self.workspace_path(workspace), &document)
            .map_err(|e| SettingsRepoError::new(e.to_string()))?;
        // The document now lives in its new place; a leftover old copy would only be read again
        // if the new one were deleted. Failing to remove it changes nothing.
        let _ = std::fs::remove_file(self.legacy_path(workspace));
        cache.workspaces.insert(workspace.clone(), Some(document));
        Ok(())
    }

    fn delete_workspace(&self, workspace: &WorkspaceId) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        // Both places, whatever the first one does: a copy left under the old name would be read
        // again.
        let mut failed = Vec::new();
        for path in [self.workspace_path(workspace), self.legacy_path(workspace)] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => failed.push(format!("cannot remove {}: {e}", path.display())),
            }
        }
        if !failed.is_empty() {
            // The cache may now disagree with the disk; the next read looks again.
            cache.workspaces.remove(workspace);
            return Err(SettingsRepoError::new(failed.join("; ")));
        }
        cache.workspaces.insert(workspace.clone(), None);
        Ok(())
    }

    fn load_app_icon(&self) -> Result<Option<Vec<u8>>, SettingsRepoError> {
        let mut cache = self.cache();
        if cache.app_icon_read {
            return Ok(cache.app_icon.clone());
        }
        let path = self.app_icon_path();
        let loaded = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(SettingsRepoError::new(format!(
                    "cannot read {}: {e}",
                    path.display()
                )));
            }
        };
        cache.app_icon.clone_from(&loaded);
        cache.app_icon_read = true;
        Ok(loaded)
    }

    fn save_app_icon(&self, png: &[u8]) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        let path = self.app_icon_path();
        puddle_fs::private::write_atomic(&path, png)
            .map_err(|e| SettingsRepoError::new(format!("cannot write {}: {e}", path.display())))?;
        cache.app_icon = Some(png.to_vec());
        cache.app_icon_read = true;
        Ok(())
    }

    fn delete_app_icon(&self) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        let path = self.app_icon_path();
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(SettingsRepoError::new(format!(
                    "cannot remove {}: {e}",
                    path.display()
                )));
            }
        }
        cache.app_icon = None;
        cache.app_icon_read = true;
        Ok(())
    }
}

/// What the book keeps for one workspace; its volume and sandbox live in the runtime. Loading
/// checks every field, so a value of this type is always valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stored {
    pub(crate) id: WorkspaceId,
    /// What the user calls the workspace: display text, nothing is keyed by it.
    pub(crate) name: puddle_api::DisplayName,
    /// The repositories Puddle manages for it, in the order they were added.
    pub(crate) repositories: Vec<Repository>,
    /// Where terminals land and what the editor opens; `None` is the volume root.
    pub(crate) start_folder: Option<StartFolder>,
    /// The guest user sessions run as.
    pub(crate) session_user: SessionUser,
    pub(crate) image: String,
    pub(crate) memory_mib: u32,
    pub(crate) created_at: u64,
    pub(crate) disk_size_mib: u64,
    /// The size in MiB the user grew the root disk to; `None` when it was never grown.
    pub(crate) root_disk_grown_mib: Option<u32>,
    /// Set from the moment a create starts until it finishes. A record still marked after a
    /// restart is an interrupted create: its volume is an orphan for reconcile to remove.
    pub(crate) creating: bool,
    /// Set from the moment a delete is confirmed until it has finished. A record still marked
    /// after a restart is an interrupted delete: the workspace is gone when its volume is, and
    /// otherwise it comes back as it was.
    pub(crate) deleting: bool,
}

/// The version the book is written in. Version 1 held one `repo_url` per workspace; version 2
/// holds a list of repositories and a start folder; version 3 holds the display text as `name`
/// and has no `display_name`, and lists the ids of deleted workspaces as `retired_ids`.
const BOOK_VERSION: u32 = 3;

/// A version 3 entry as it is on disk.
#[derive(Debug, Serialize, Deserialize)]
struct EntryV3 {
    id: String,
    name: String,
    repositories: Vec<RepositoryV2>,
    #[serde(default)]
    start_folder: Option<String>,
    /// Missing in an entry written before workspaces had a session user: those keep root.
    #[serde(default = "root")]
    session_user: SessionUser,
    image: String,
    memory_mib: u32,
    created_at: u64,
    disk_size_mib: u64,
    /// Missing in an entry of a workspace whose root disk was never grown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root_disk_grown_mib: Option<u32>,
    #[serde(default)]
    creating: bool,
    /// Missing in an entry written before a delete was saved as it began.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    deleting: bool,
}

const fn root() -> SessionUser {
    SessionUser::Root
}

/// A repository row of versions 1 to 3. `entry` is missing in a list written before entries
/// had ids: a read gives such an entry a fresh id and the next save writes it.
#[derive(Debug, Serialize, Deserialize)]
struct RepositoryV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    entry: Option<String>,
    url: String,
    directory: String,
}

/// A version 1 entry: one repository, cloned into the directory its address gives. Read only.
#[derive(Debug, Deserialize)]
struct EntryV1 {
    id: String,
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    repo_url: String,
    image: String,
    memory_mib: u32,
    created_at: u64,
    disk_size_mib: u64,
    #[serde(default)]
    creating: bool,
}

/// A version 2 entry: a DNS-label `name` and an optional `display_name`. Read only.
#[derive(Debug, Deserialize)]
struct EntryV2 {
    id: String,
    name: String,
    #[serde(default)]
    display_name: Option<String>,
    repositories: Vec<RepositoryV2>,
    #[serde(default)]
    start_folder: Option<String>,
    #[serde(default = "root")]
    session_user: SessionUser,
    image: String,
    memory_mib: u32,
    created_at: u64,
    disk_size_mib: u64,
    #[serde(default)]
    creating: bool,
}

impl From<EntryV2> for EntryV3 {
    /// The workspace as version 3 says it: the old display name, else the old name, is the
    /// name. The id stays what it was.
    fn from(v2: EntryV2) -> Self {
        Self {
            id: v2.id,
            name: v2.display_name.unwrap_or(v2.name),
            repositories: v2.repositories,
            start_folder: v2.start_folder,
            session_user: v2.session_user,
            image: v2.image,
            memory_mib: v2.memory_mib,
            created_at: v2.created_at,
            disk_size_mib: v2.disk_size_mib,
            root_disk_grown_mib: None,
            creating: v2.creating,
            deleting: false,
        }
    }
}

impl From<EntryV1> for EntryV2 {
    /// The workspace as version 2 says it: its one repository as a one-entry list in the
    /// directory the clone made, and that checkout as its start folder, so terminals and the
    /// editor open where they did.
    fn from(v1: EntryV1) -> Self {
        let directory = CheckoutDir::from_url(&v1.repo_url).to_string();
        Self {
            id: v1.id,
            name: v1.name,
            display_name: v1.display_name,
            repositories: vec![RepositoryV2 {
                entry: None,
                url: v1.repo_url,
                directory: directory.clone(),
            }],
            start_folder: Some(directory),
            session_user: SessionUser::Root,
            image: v1.image,
            memory_mib: v1.memory_mib,
            created_at: v1.created_at,
            disk_size_mib: v1.disk_size_mib,
            creating: v1.creating,
        }
    }
}

impl EntryV3 {
    fn check(self) -> Result<Stored, BookError> {
        let id = WorkspaceId::new(&self.id).map_err(|e| bad_entry(&self.id, "has a bad id", e))?;
        let name = puddle_api::DisplayName::parse(&self.name)
            .map_err(|e| bad_entry(&self.id, "has a bad name", e))?;
        let mut taken: Vec<EntryId> = Vec::new();
        for given in self.repositories.iter().filter_map(|r| r.entry.as_deref()) {
            let entry = EntryId::new(given)
                .map_err(|e| bad_entry(&self.id, "has a bad repository entry id", e))?;
            if taken.contains(&entry) {
                return Err(bad_entry(
                    &self.id,
                    "lists one repository entry id twice",
                    DuplicateEntry(entry),
                ));
            }
            taken.push(entry);
        }
        let mut repositories = Vec::with_capacity(self.repositories.len());
        for r in &self.repositories {
            let entry = if let Some(given) = &r.entry {
                EntryId::new(given)
                    .map_err(|e| bad_entry(&self.id, "has a bad repository entry id", e))?
            } else {
                let fresh = puddle_api::new_entry_id(|e| taken.contains(e))
                    .map_err(|e| bad_entry(&self.id, "could not give a repository an id", e))?;
                taken.push(fresh.clone());
                fresh
            };
            repositories.push(Repository {
                entry,
                url: RepoUrl::parse(&r.url)
                    .map_err(|e| bad_entry(&self.id, "has a bad repository address", e))?,
                directory: CheckoutDir::new(&r.directory)
                    .map_err(|e| bad_entry(&self.id, "has a bad checkout directory", e))?,
            });
        }
        let start_folder = self
            .start_folder
            .as_deref()
            .map(StartFolder::parse)
            .transpose()
            .map_err(|e| bad_entry(&self.id, "has a bad start folder", e))?;
        Ok(Stored {
            id,
            name,
            repositories,
            start_folder,
            session_user: self.session_user,
            image: self.image,
            memory_mib: self.memory_mib,
            created_at: self.created_at,
            disk_size_mib: self.disk_size_mib,
            root_disk_grown_mib: self.root_disk_grown_mib,
            creating: self.creating,
            deleting: self.deleting,
        })
    }
}

impl From<Stored> for EntryV3 {
    fn from(s: Stored) -> Self {
        Self {
            id: s.id.to_string(),
            name: s.name.to_string(),
            repositories: s
                .repositories
                .into_iter()
                .map(|r| RepositoryV2 {
                    entry: Some(r.entry.to_string()),
                    url: r.url.as_str().to_owned(),
                    directory: r.directory.to_string(),
                })
                .collect(),
            start_folder: s.start_folder.map(|f| f.to_string()),
            session_user: s.session_user,
            image: s.image,
            memory_mib: s.memory_mib,
            created_at: s.created_at,
            disk_size_mib: s.disk_size_mib,
            root_disk_grown_mib: s.root_disk_grown_mib,
            creating: s.creating,
            deleting: s.deleting,
        }
    }
}

#[derive(Debug, Deserialize)]
struct Version {
    version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
struct BookFile<E> {
    version: u32,
    workspaces: Vec<E>,
    /// The ids of deleted workspaces, kept for good so no new workspace gets one (version 3;
    /// none in an older book).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    retired_ids: Vec<String>,
}

/// What the book holds: the workspaces, and the ids no new workspace may get.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Book {
    pub(crate) workspaces: Vec<Stored>,
    pub(crate) retired: BTreeSet<WorkspaceId>,
}

/// The workspace book: a JSON file with one entry per workspace.
#[derive(Debug, Clone)]
pub(crate) struct WorkspaceBook {
    path: PathBuf,
}

impl WorkspaceBook {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn error(reason: impl std::fmt::Display) -> HostError {
        HostError::State {
            what: "the workspace list",
            reason: reason.to_string(),
        }
    }

    /// Every stored workspace; none when there is no book yet. A missing book costs no
    /// volume: reconcile keeps every workspace volume the book doesn't name, and the host logs it.
    /// A version 1 or 2 book is read as version 3 and stays on disk as it is until the next save; a
    /// book newer than this Puddle knows is refused, so it is never read wrongly or overwritten.
    pub(crate) fn load(&self) -> Result<Book, HostError> {
        self.read_book().map_err(Self::error)
    }

    fn read_book(&self) -> Result<Book, BookError> {
        let Some(value) = read_json(&self.path)? else {
            return Ok(Book::default());
        };
        let not_a_list = |source: serde_json::Error| BookError::NotAList {
            path: self.path.clone(),
            source,
        };
        let version = Version::deserialize(&value).map_err(not_a_list)?.version;
        let (entries, retired): (Vec<EntryV3>, Vec<String>) = match version {
            1 => (
                BookFile::<EntryV1>::deserialize(&value)
                    .map_err(not_a_list)?
                    .workspaces
                    .into_iter()
                    .map(|e| EntryV3::from(EntryV2::from(e)))
                    .collect(),
                Vec::new(),
            ),
            2 => (
                BookFile::<EntryV2>::deserialize(&value)
                    .map_err(not_a_list)?
                    .workspaces
                    .into_iter()
                    .map(EntryV3::from)
                    .collect(),
                Vec::new(),
            ),
            BOOK_VERSION => {
                let file = BookFile::<EntryV3>::deserialize(&value).map_err(not_a_list)?;
                (file.workspaces, file.retired_ids)
            }
            newer if newer > BOOK_VERSION => {
                return Err(BookError::Newer {
                    path: self.path.clone(),
                    version: newer,
                });
            }
            older => {
                return Err(BookError::Unknown {
                    path: self.path.clone(),
                    version: older,
                });
            }
        };
        let workspaces = entries
            .into_iter()
            .map(EntryV3::check)
            .collect::<Result<_, _>>()?;
        let retired = retired
            .iter()
            .map(|id| {
                WorkspaceId::new(id).map_err(|source| BookError::RetiredId {
                    id: id.clone(),
                    source,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Book {
            workspaces,
            retired,
        })
    }

    pub(crate) fn save(
        &self,
        workspaces: Vec<Stored>,
        retired: &BTreeSet<WorkspaceId>,
    ) -> Result<(), HostError> {
        let file = BookFile {
            version: BOOK_VERSION,
            workspaces: workspaces
                .into_iter()
                .map(EntryV3::from)
                .collect::<Vec<_>>(),
            retired_ids: retired.iter().map(ToString::to_string).collect(),
        };
        let value = serde_json::to_value(&file)
            .map_err(StateFileError::Encode)
            .map_err(Self::error)?;
        write_json(&self.path, &value).map_err(Self::error)
    }
}

#[cfg(test)]
mod tests;
