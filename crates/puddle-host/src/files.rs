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
use puddle_types::{CheckoutDir, SessionUser, WorkspaceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::HostError;

/// Reads `path` as JSON, `None` when it does not exist.
fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("{} is not valid JSON: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    puddle_fs::private::write_atomic(path, &body)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
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
        let loaded = read_json(&self.global_path()).map_err(SettingsRepoError::new)?;
        cache.global.clone_from(&loaded);
        cache.global_read = true;
        Ok(loaded)
    }

    fn save_global(&self, document: Value) -> Result<(), SettingsRepoError> {
        let mut cache = self.cache();
        write_json(&self.global_path(), &document).map_err(SettingsRepoError::new)?;
        cache.global = Some(document);
        cache.global_read = true;
        Ok(())
    }

    fn load_workspace(&self, workspace: &WorkspaceId) -> Result<Option<Value>, SettingsRepoError> {
        let mut cache = self.cache();
        if let Some(cached) = cache.workspaces.get(workspace) {
            return Ok(cached.clone());
        }
        let mut loaded =
            read_json(&self.workspace_path(workspace)).map_err(SettingsRepoError::new)?;
        if loaded.is_none() {
            loaded = read_json(&self.legacy_path(workspace)).map_err(SettingsRepoError::new)?;
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
        write_json(&self.workspace_path(workspace), &document).map_err(SettingsRepoError::new)?;
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
}

const fn root() -> SessionUser {
    SessionUser::Root
}

#[derive(Debug, Serialize, Deserialize)]
struct RepositoryV2 {
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
    fn check(self) -> Result<Stored, String> {
        let at =
            |what: &str, e: &dyn std::fmt::Display| format!("workspace {:?} {what}: {e}", self.id);
        let id = WorkspaceId::new(&self.id).map_err(|e| at("has a bad id", &e))?;
        let name =
            puddle_api::DisplayName::parse(&self.name).map_err(|e| at("has a bad name", &e))?;
        let repositories = self
            .repositories
            .iter()
            .map(|r| {
                Ok(Repository {
                    url: RepoUrl::parse(&r.url)
                        .map_err(|e| at("has a bad repository address", &e))?,
                    directory: CheckoutDir::new(&r.directory)
                        .map_err(|e| at("has a bad checkout directory", &e))?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let start_folder = self
            .start_folder
            .as_deref()
            .map(StartFolder::parse)
            .transpose()
            .map_err(|e| at("has a bad start folder", &e))?;
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

    fn error(reason: String) -> HostError {
        HostError::State {
            what: "the workspace list",
            reason,
        }
    }

    /// Every stored workspace; none when there is no book yet. A missing book costs no
    /// volume: reconcile keeps every workspace volume the book doesn't name, and the host logs it.
    /// A version 1 or 2 book is read as version 3 and stays on disk as it is until the next save; a
    /// book newer than this Puddle knows is refused, so it is never read wrongly or overwritten.
    pub(crate) fn load(&self) -> Result<Book, HostError> {
        let Some(value) = read_json(&self.path).map_err(Self::error)? else {
            return Ok(Book::default());
        };
        let not_a_list = |e: serde_json::Error| {
            Self::error(format!(
                "{} is not a workspace list: {e}",
                self.path.display()
            ))
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
                return Err(Self::error(format!(
                    "{} was written by a newer Puddle (version {newer})",
                    self.path.display()
                )));
            }
            older => {
                return Err(Self::error(format!(
                    "{} has version {older}, which no Puddle wrote",
                    self.path.display()
                )));
            }
        };
        let workspaces = entries
            .into_iter()
            .map(|entry| entry.check().map_err(Self::error))
            .collect::<Result<_, _>>()?;
        let retired = retired
            .iter()
            .map(|id| {
                WorkspaceId::new(id)
                    .map_err(|e| Self::error(format!("retired id {id:?} is not an id: {e}")))
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
        let value = serde_json::to_value(&file).map_err(|e| Self::error(e.to_string()))?;
        write_json(&self.path, &value).map_err(Self::error)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn repository(url: &str, dir: &str) -> Repository {
        Repository {
            url: RepoUrl::parse(url).unwrap(),
            directory: CheckoutDir::new(dir).unwrap(),
        }
    }

    fn stored(id: &str) -> Stored {
        Stored {
            id: WorkspaceId::new(id).unwrap(),
            name: puddle_api::DisplayName::parse(id).unwrap(),
            repositories: vec![
                repository("https://example.org/a/b", "b"),
                repository("https://example.org/a/c.git", "see"),
            ],
            start_folder: Some(StartFolder::parse("see/src").unwrap()),
            session_user: SessionUser::Agent,
            image: "alpine:3".to_owned(),
            memory_mib: 1024,
            created_at: 7,
            disk_size_mib: 2048,
            root_disk_grown_mib: Some(32_768),
            creating: false,
        }
    }

    #[test]
    fn settings_round_trip_and_a_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FileSettings::new(dir.path().join("settings"));
        let a = WorkspaceId::new("a").unwrap();
        assert_eq!(repo.load_global().unwrap(), None);
        assert_eq!(repo.load_workspace(&a).unwrap(), None);
        repo.save_global(json!({"schema_version": 1})).unwrap();
        repo.save_workspace(&a, json!({"overrides": {}})).unwrap();
        assert_eq!(
            repo.load_global().unwrap(),
            Some(json!({"schema_version": 1}))
        );
        assert_eq!(
            repo.load_workspace(&a).unwrap(),
            Some(json!({"overrides": {}}))
        );
        // A second instance over the same folder sees the same documents.
        let again = FileSettings::new(dir.path().join("settings"));
        assert_eq!(
            again.load_global().unwrap(),
            Some(json!({"schema_version": 1}))
        );
    }

    #[test]
    fn deleting_a_workspace_document_removes_both_copies_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceId::new("a").unwrap();
        let repo = FileSettings::new(dir.path());
        // Nothing stored: not an error.
        repo.delete_workspace(&a).unwrap();
        repo.save_workspace(&a, json!({"overrides": {"direct_ssh": true}}))
            .unwrap();
        let old = dir.path().join("sandboxes");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.json"), br#"{"overrides":{"cpus":2}}"#).unwrap();
        repo.delete_workspace(&a).unwrap();
        assert!(!dir.path().join("workspaces").join("a.json").exists());
        assert!(!old.join("a.json").exists());
        assert_eq!(repo.load_workspace(&a).unwrap(), None);
        assert_eq!(
            FileSettings::new(dir.path()).load_workspace(&a).unwrap(),
            None
        );
    }

    #[test]
    fn a_workspace_document_that_cannot_be_removed_is_an_error_and_the_other_copy_still_goes() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceId::new("a").unwrap();
        let repo = FileSettings::new(dir.path());
        // A non-empty folder where the file belongs: removing it fails on every OS.
        let blocked = dir.path().join("workspaces").join("a.json");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), b"x").unwrap();
        let old = dir.path().join("sandboxes");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.json"), b"{}").unwrap();
        let err = repo.delete_workspace(&a).unwrap_err();
        assert!(err.to_string().contains("a.json"), "{err}");
        assert!(!old.join("a.json").exists());
    }

    #[test]
    fn a_failed_delete_does_not_leave_the_cache_claiming_the_old_document() {
        let dir = tempfile::tempdir().unwrap();
        let a = WorkspaceId::new("a").unwrap();
        let repo = FileSettings::new(dir.path());
        repo.save_workspace(&a, json!({"overrides": {}})).unwrap();
        let blocked = dir.path().join("sandboxes").join("a.json");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), b"x").unwrap();
        repo.delete_workspace(&a).unwrap_err();
        // The document is gone and the old copy cannot be read: the next load says so.
        repo.load_workspace(&a).unwrap_err();
    }

    #[test]
    fn a_document_kept_under_the_old_folder_name_is_read_and_moves_on_save() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("sandboxes");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("a.json"), br#"{"overrides":{"cpus":2}}"#).unwrap();
        let a = WorkspaceId::new("a").unwrap();
        let repo = FileSettings::new(dir.path());
        assert_eq!(
            repo.load_workspace(&a).unwrap(),
            Some(json!({"overrides": {"cpus": 2}}))
        );
        repo.save_workspace(&a, json!({"overrides": {"cpus": 4}}))
            .unwrap();
        assert!(!old.join("a.json").exists());
        assert!(dir.path().join("workspaces").join("a.json").exists());
        let again = FileSettings::new(dir.path());
        assert_eq!(
            again.load_workspace(&a).unwrap(),
            Some(json!({"overrides": {"cpus": 4}}))
        );
    }

    #[test]
    fn a_damaged_settings_file_is_an_error_not_an_empty_document() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("global.json"), b"{ nope").unwrap();
        let err = FileSettings::new(dir.path()).load_global().unwrap_err();
        assert!(err.to_string().contains("not valid JSON"), "{err}");
    }

    #[test]
    fn the_app_icon_round_trips_survives_a_restart_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = FileSettings::new(dir.path().join("settings"));
        assert_eq!(repo.load_app_icon().unwrap(), None);
        repo.delete_app_icon().unwrap();
        repo.save_app_icon(b"one").unwrap();
        assert_eq!(repo.load_app_icon().unwrap(), Some(b"one".to_vec()));
        let again = FileSettings::new(dir.path().join("settings"));
        assert_eq!(again.load_app_icon().unwrap(), Some(b"one".to_vec()));
        repo.save_app_icon(b"two").unwrap();
        assert_eq!(repo.load_app_icon().unwrap(), Some(b"two".to_vec()));
        repo.delete_app_icon().unwrap();
        assert_eq!(repo.load_app_icon().unwrap(), None);
        assert!(!dir.path().join("settings").join("app-icon.png").exists());
    }

    #[test]
    fn an_app_icon_that_cannot_be_read_written_or_removed_says_so() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file belongs: reading and removing it fail.
        std::fs::create_dir(dir.path().join("app-icon.png")).unwrap();
        let repo = FileSettings::new(dir.path());
        assert!(
            repo.load_app_icon()
                .unwrap_err()
                .to_string()
                .contains("cannot read")
        );
        assert!(
            repo.delete_app_icon()
                .unwrap_err()
                .to_string()
                .contains("cannot remove")
        );
        assert!(
            repo.save_app_icon(b"x")
                .unwrap_err()
                .to_string()
                .contains("cannot write")
        );
    }

    #[test]
    fn a_settings_folder_that_is_a_file_cannot_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("settings");
        std::fs::write(&blocker, b"x").unwrap();
        let err = FileSettings::new(&blocker)
            .save_global(json!({}))
            .unwrap_err();
        assert!(err.to_string().contains("cannot write"), "{err}");
    }

    #[test]
    fn the_book_round_trips_and_a_missing_book_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let book = WorkspaceBook::new(dir.path().join("workspaces.json"));
        assert_eq!(book.load().unwrap().workspaces, Vec::new());
        book.save(vec![stored("a"), stored("b")], &BTreeSet::new())
            .unwrap();
        assert_eq!(
            book.load().unwrap().workspaces,
            vec![stored("a"), stored("b")]
        );
    }

    #[test]
    fn a_damaged_or_newer_book_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let book = WorkspaceBook::new(&path);
        std::fs::write(&path, b"[1,2").unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("not valid JSON")
        );
        std::fs::write(&path, b"{\"nope\": true}").unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("not a workspace list")
        );
        std::fs::write(&path, br#"{"version": 99, "workspaces": []}"#).unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("newer Puddle")
        );
        std::fs::write(&path, br#"{"version": 4, "workspaces": []}"#).unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("written by a newer Puddle (version 4)")
        );
        std::fs::write(&path, br#"{"version": 0, "workspaces": []}"#).unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("no Puddle wrote")
        );
        let bad = json!({"version": 1, "workspaces": [{
            "id": "Not A Name", "name": "x", "repo_url": "u", "image": "i", "memory_mib": 1,
            "created_at": 0, "disk_size_mib": 1, "first_connect_notice_due": false
        }]});
        std::fs::write(&path, bad.to_string()).unwrap();
        assert!(book.load().is_err());
        for (field, value, why) in [
            ("url", json!("git@h:a/b"), "bad repository address"),
            ("directory", json!("../up"), "bad checkout directory"),
        ] {
            let mut entry = json!({"url": "https://h/a/b", "directory": "b"});
            entry[field] = value;
            let bad = json!({"version": 2, "workspaces": [{
                "id": "a", "name": "a", "repositories": [entry], "start_folder": null,
                "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
            }]});
            std::fs::write(&path, bad.to_string()).unwrap();
            let err = book.load().unwrap_err().to_string();
            assert!(err.contains(why), "{err}");
        }
        let bad = json!({"version": 2, "workspaces": [{
            "id": "a", "name": "a", "repositories": [], "start_folder": "/etc",
            "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
        }]});
        std::fs::write(&path, bad.to_string()).unwrap();
        assert!(
            book.load()
                .unwrap_err()
                .to_string()
                .contains("bad start folder")
        );
    }

    /// A version 1 book as Puddle wrote it before repository lists: one entry still creating, one
    /// finished from before the creating flag existed (with the retired first-connect flag), one
    /// whose address gave no usable directory name.
    const V1_BOOK: &str = r#"{
  "version": 1,
  "workspaces": [
    {
      "id": "shop",
      "name": "shop",
      "display_name": "Web shop",
      "repo_url": "https://github.com/acme/shop.git",
      "image": "mcr.microsoft.com/devcontainers/base:debian",
      "memory_mib": 8192,
      "created_at": 1760000000000,
      "disk_size_mib": 32768,
      "creating": false
    },
    {
      "id": "old",
      "name": "old",
      "repo_url": "https://example.org/team/tools",
      "image": "alpine:3",
      "memory_mib": 512,
      "created_at": 1,
      "disk_size_mib": 64,
      "first_connect_notice_due": true
    },
    {
      "id": "odd",
      "name": "odd",
      "repo_url": "https://example.org/team/.hidden",
      "image": "alpine:3",
      "memory_mib": 512,
      "created_at": 2,
      "disk_size_mib": 64,
      "creating": true
    }
  ]
}"#;

    /// Per workspace: id, (address, directory) of each repository, start folder, creating.
    type Shape = Vec<(String, Vec<(String, String)>, Option<String>, bool)>;

    fn shape(loaded: &[Stored]) -> Shape {
        loaded
            .iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.repositories
                        .iter()
                        .map(|r| (r.url.to_string(), r.directory.to_string()))
                        .collect(),
                    s.start_folder.as_ref().map(ToString::to_string),
                    s.creating,
                )
            })
            .collect()
    }

    fn v1_shape() -> Shape {
        let one = |url: &str, dir: &str| vec![(url.to_owned(), dir.to_owned())];
        vec![
            (
                "shop".into(),
                one("https://github.com/acme/shop.git", "shop"),
                Some("shop".into()),
                false,
            ),
            (
                "old".into(),
                one("https://example.org/team/tools", "tools"),
                Some("tools".into()),
                false,
            ),
            (
                "odd".into(),
                one("https://example.org/team/.hidden", "src"),
                Some("src".into()),
                true,
            ),
        ]
    }

    #[test]
    fn a_version_1_book_loads_as_lists_with_the_checkout_as_start_folder_and_is_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        std::fs::write(&path, V1_BOOK).unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let book = WorkspaceBook::new(&path);
        let loaded = book.load().unwrap().workspaces;
        assert_eq!(shape(&loaded), v1_shape());
        let shop = &loaded[0];
        assert_eq!(shop.name.as_str(), "Web shop");
        assert_eq!(loaded[1].name.as_str(), "old");
        assert_eq!(
            (
                shop.image.as_str(),
                shop.memory_mib,
                shop.created_at,
                shop.disk_size_mib
            ),
            (
                "mcr.microsoft.com/devcontainers/base:debian",
                8192,
                1_760_000_000_000,
                32768
            )
        );
        book.load().unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            V1_BOOK,
            "a read never writes"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );

        // The next save writes version 3, without the old field, and it reads back the same.
        book.save(loaded.clone(), &BTreeSet::new()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 3);
        assert_eq!(written["workspaces"][0]["name"], "Web shop");
        assert_eq!(
            written["workspaces"][0]["repositories"],
            json!([{"url": "https://github.com/acme/shop.git", "directory": "shop"}])
        );
        assert_eq!(written["workspaces"][0]["start_folder"], "shop");
        for entry in written["workspaces"].as_array().unwrap() {
            assert!(entry.get("repo_url").is_none(), "{entry}");
            assert!(entry.get("display_name").is_none(), "{entry}");
            assert!(entry.get("first_connect_notice_due").is_none(), "{entry}");
        }
        assert_eq!(book.load().unwrap().workspaces, loaded);
    }

    #[test]
    fn an_entry_from_before_session_users_keeps_root_and_the_user_is_saved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let book = WorkspaceBook::new(&path);
        let old = json!({"version": 2, "workspaces": [{
            "id": "a", "name": "a", "repositories": [], "start_folder": null,
            "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
        }]});
        std::fs::write(&path, old.to_string()).unwrap();
        let loaded = book.load().unwrap().workspaces;
        assert_eq!(loaded[0].session_user, SessionUser::Root);
        book.save(vec![loaded[0].clone(), stored("b")], &BTreeSet::new())
            .unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["workspaces"][0]["session_user"], "root");
        assert_eq!(written["workspaces"][1]["session_user"], "agent");
        let bad = json!({"version": 2, "workspaces": [{
            "id": "a", "name": "a", "repositories": [], "session_user": "vscode",
            "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
        }]});
        std::fs::write(&path, bad.to_string()).unwrap();
        assert!(book.load().is_err());
        // A version 1 book is older still: every workspace in it keeps root.
        std::fs::write(&path, V1_BOOK).unwrap();
        assert!(
            book.load()
                .unwrap()
                .workspaces
                .iter()
                .all(|w| w.session_user == SessionUser::Root)
        );
    }

    #[test]
    fn a_version_1_book_written_by_a_puddle_before_repository_lists_loads() {
        // Written by `WorkspaceBook::save` of the version before repository lists, unchanged.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        std::fs::write(&path, include_str!("../tests/fixtures/workspaces-v1.json")).unwrap();
        let loaded = WorkspaceBook::new(&path).load().unwrap().workspaces;
        assert_eq!(shape(&loaded), v1_shape());
    }

    #[test]
    fn a_version_2_book_written_by_the_previous_release_loads_in_place_and_saves_as_version_3() {
        // Written by `WorkspaceBook::save` of the release before display text names, unchanged.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let old = include_str!("../tests/fixtures/workspaces-v2.json");
        std::fs::write(&path, old).unwrap();
        let book = WorkspaceBook::new(&path);
        let loaded = book.load().unwrap().workspaces;
        // Each entry keeps its id; the name is the old display name, else the old name.
        assert_eq!(
            loaded
                .iter()
                .map(|s| (s.id.as_str(), s.name.as_str()))
                .collect::<Vec<_>>(),
            [("shop", "Shop API \u{fc}"), ("tools", "tools")]
        );
        assert_eq!(loaded[0].repositories.len(), 2);
        assert_eq!(loaded[0].memory_mib, 8192);
        assert_eq!(loaded[1].session_user, SessionUser::Root);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            old,
            "a read never writes"
        );
        book.save(loaded.clone(), &BTreeSet::new()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["version"], 3);
        assert_eq!(written["workspaces"][0]["id"], "shop");
        assert_eq!(written["workspaces"][0]["name"], "Shop API \u{fc}");
        assert!(written["workspaces"][0].get("display_name").is_none());
        assert_eq!(book.load().unwrap().workspaces, loaded);
        // A display name that is no longer valid text stops the start with its workspace named.
        let bad = old.replace("Shop API \u{fc}", "two\\nlines");
        std::fs::write(&path, bad).unwrap();
        let err = book.load().unwrap_err().to_string();
        assert!(
            err.contains("\"shop\"") && err.contains("bad name"),
            "{err}"
        );
    }

    #[test]
    fn a_version_3_book_round_trips_with_empty_and_long_lists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let book = WorkspaceBook::new(&path);
        let mut empty = stored("empty");
        empty.repositories.clear();
        empty.start_folder = None;
        let mut many = stored("many");
        many.repositories = (0..100)
            .map(|n| repository(&format!("https://h/a/r{n}"), &format!("r{n}")))
            .collect();
        let all = vec![stored("a"), empty, many];
        book.save(all.clone(), &BTreeSet::new()).unwrap();
        assert_eq!(book.load().unwrap().workspaces, all);
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["workspaces"][1]["repositories"], json!([]));
        assert_eq!(written["workspaces"][1]["start_folder"], Value::Null);
    }

    #[test]
    fn retired_ids_are_kept_beside_the_workspaces_and_a_bad_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workspaces.json");
        let book = WorkspaceBook::new(&path);
        let retired: BTreeSet<WorkspaceId> = ["acme", "k3m9q2xa"]
            .into_iter()
            .map(|id| WorkspaceId::new(id).unwrap())
            .collect();
        book.save(vec![stored("a")], &retired).unwrap();
        let loaded = book.load().unwrap();
        assert_eq!(loaded.workspaces, vec![stored("a")]);
        assert_eq!(loaded.retired, retired);
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["retired_ids"], json!(["acme", "k3m9q2xa"]));
        // None retired: the field is left out, and a book without it has none.
        book.save(vec![stored("a")], &BTreeSet::new()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written.get("retired_ids"), None);
        assert!(book.load().unwrap().retired.is_empty());
        std::fs::write(
            &path,
            br#"{"version": 3, "workspaces": [], "retired_ids": ["Not An Id"]}"#,
        )
        .unwrap();
        let err = book.load().unwrap_err().to_string();
        assert!(
            err.contains("retired id \"Not An Id\" is not an id"),
            "{err}"
        );
    }
}
