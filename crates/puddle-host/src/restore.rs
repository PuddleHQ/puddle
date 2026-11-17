// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore, first half: reading a backup file or folder into a private staging folder and refusing
//! one that cannot be read faithfully (backup page, BK-10 to BK-12).
//!
//! Staging writes only below the staging root the caller names. Nothing in the data folder's live
//! files is read for writing or changed: a restore applies later, at a start, from the staged parts.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, BufRead as _, BufReader, Read};
use std::path::{Component, Path, PathBuf};

use puddle_api::wire::{RestorePart, RestorePartName};
use puddle_api::{RestoreFacts, RestoreServiceError, SecretRef};
use puddle_fs::private;
use puddle_store::DatabaseRefusal;

use crate::HostPaths;
use apply::ApplyMode;

use crate::backup::{
    BACKUP_DATABASE, BACKUP_FORMAT, BACKUP_MANIFEST, BACKUP_SETTINGS, BACKUP_WORKSPACES, Manifest,
};

/// The most a backup may unpack to, so a small file cannot fill the disk (BK-12).
pub const MAX_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Why a backup was refused. Each message names the reason and the way on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refusal {
    /// Not a gzip file.
    #[error("this is not a gzip archive; choose a file made by Puddle's \"Back up now\"")]
    NotGzip,
    /// Gzip, but not a readable tar archive.
    #[error("the archive is damaged or is not a Puddle backup: {0}")]
    Damaged(String),
    /// A name outside `puddle.db`, `settings`, `workspaces.json` and `manifest.json`.
    #[error("the backup holds {0:?}, which a Puddle backup never does; it was not restored")]
    UnknownEntry(String),
    /// An absolute path, or one that climbs above the top level.
    #[error("the backup has the path {0:?}, which points outside the backup; it was not restored")]
    UnsafePath(String),
    /// A symbolic or hard link.
    #[error("{0:?} in the backup is a link; a Puddle backup holds plain files only")]
    Link(String),
    /// The same name twice.
    #[error("{0:?} appears twice in the backup")]
    Duplicate(String),
    /// More than [`MAX_UNPACKED_BYTES`].
    #[error("the backup unpacks to more than the limit of 4 GiB; it was not restored")]
    TooLarge,
    /// No `puddle.db`.
    #[error("the backup has no puddle.db; it is not a Puddle backup")]
    NoDatabase,
    /// The database was refused.
    #[error("{0}")]
    Database(DatabaseRefusal),
    /// `manifest.json` could not be read.
    #[error("manifest.json cannot be read: {0}")]
    BadManifest(String),
    /// A format number this Puddle does not know.
    #[error("the backup is in format {0}, which this Puddle does not know; update Puddle first")]
    UnknownFormat(u32),
}

/// Why staging did not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreError {
    /// The backup cannot be read faithfully.
    #[error("{0}")]
    Refused(#[from] Refusal),
    /// A file or folder could not be read or written.
    #[error("{what}: {source}")]
    Io {
        /// What was being done, with the path.
        what: String,
        /// What went wrong.
        source: io::Error,
    },
    /// The backup cannot be merged with what is here, and why.
    #[error("{0}")]
    Merge(String),
    /// The staged database could not be migrated.
    #[error("cannot read the backup's database: {0}")]
    Store(#[from] puddle_store::StoreError),
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> RestoreError {
    let what = format!("{what} {}", path.display());
    move |source| RestoreError::Io { what, source }
}

/// A backup that passed every check, in a private folder of its own. Remove the folder with
/// [`discard`] when it is not applied.
#[derive(Debug)]
pub struct Staged {
    /// The staging id: the folder's name below the staging root.
    pub id: String,
    /// The folder holding the parts, with the database already at this Puddle's schema.
    pub dir: PathBuf,
    /// `manifest.json`, when the backup has one (a backup without one is format 1).
    pub manifest: Option<Manifest>,
    /// The schema version the backup's database had before it was migrated.
    pub schema_version: u32,
}

/// Where a staging id's folder is below `root`; `None` when `id` is not one this module made.
#[must_use]
pub fn staging_dir(root: &Path, id: &str) -> Option<PathBuf> {
    let ok = id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    ok.then(|| root.join(id))
}

/// Removes the staging folder `dir`; best effort, since the caller reports the reason it discards.
pub fn discard(dir: &Path) {
    drop(fs::remove_dir_all(dir));
}

/// Reads the gzip-compressed tar archive `source` into a new staging folder below `root` and checks
/// it.
///
/// # Errors
///
/// [`RestoreError::Refused`] with the reason when the backup cannot be read faithfully (nothing
/// stays staged), [`RestoreError::Io`] when the staging folder cannot be written.
pub fn stage_archive(source: impl Read, root: &Path) -> Result<Staged, RestoreError> {
    stage_archive_limited(source, root, MAX_UNPACKED_BYTES)
}

fn stage_archive_limited(
    source: impl Read,
    root: &Path,
    limit: u64,
) -> Result<Staged, RestoreError> {
    staged_in(root, |dir| unpack(source, dir, limit))
}

/// Reads the backup folder `folder` (the form `puddle backup <dest>` writes) into a new staging
/// folder below `root` and checks it. The folder is only read.
///
/// # Errors
///
/// As [`stage_archive`].
pub fn stage_folder(folder: &Path, root: &Path) -> Result<Staged, RestoreError> {
    staged_in(root, |dir| copy_folder(folder, dir, MAX_UNPACKED_BYTES))
}

/// A new staging id: 32 lowercase hex digits from `random`, the system's source.
fn staging_id(
    random: impl FnOnce(&mut [u8]) -> Result<(), getrandom::Error>,
) -> Result<String, RestoreError> {
    let mut raw = [0_u8; 16];
    random(&mut raw).map_err(|e| RestoreError::Io {
        what: "cannot make a staging id".into(),
        source: io::Error::other(e.to_string()),
    })?;
    Ok(raw.iter().fold(String::with_capacity(32), |mut id, b| {
        // Writing to a String never fails.
        let _ = write!(id, "{b:02x}");
        id
    }))
}

/// Makes the staging folder, fills it with `fill`, checks it, and removes it again on any failure.
fn staged_in(
    root: &Path,
    fill: impl FnOnce(&Path) -> Result<(), RestoreError>,
) -> Result<Staged, RestoreError> {
    private::create_dir(root).map_err(io_error("cannot create", root))?;
    let id = staging_id(getrandom::fill)?;
    let dir = root.join(&id);
    private::create_new_dir(&dir).map_err(io_error("cannot create", &dir))?;
    let result = fill(&dir).and_then(|()| check(&dir, id));
    if result.is_err() {
        discard(&dir);
    }
    result
}

fn check(dir: &Path, id: String) -> Result<Staged, RestoreError> {
    let db = dir.join(BACKUP_DATABASE);
    if !db.is_file() {
        return Err(Refusal::NoDatabase.into());
    }
    let manifest = match fs::read(dir.join(BACKUP_MANIFEST)) {
        Ok(body) => Some(
            serde_json::from_slice::<Manifest>(&body)
                .map_err(|e| Refusal::BadManifest(e.to_string()))?,
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error("cannot read", &dir.join(BACKUP_MANIFEST))(e)),
    };
    if let Some(m) = &manifest
        && m.format != BACKUP_FORMAT
    {
        return Err(Refusal::UnknownFormat(m.format).into());
    }
    let database = puddle_store::check_staged_database(&db)?.map_err(Refusal::Database)?;
    Ok(Staged {
        id,
        dir: dir.to_path_buf(),
        manifest,
        schema_version: database.schema_version,
    })
}

/// The parts of a backup that sit at the top level as one plain file.
const FILES: [&str; 3] = [BACKUP_DATABASE, BACKUP_WORKSPACES, BACKUP_MANIFEST];

/// The relative path `name` as its normal components, or why it is refused. `name` is as the
/// archive wrote it, for the message.
fn components(name: &str) -> Result<Vec<String>, Refusal> {
    let mut out = Vec::new();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => out.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(Refusal::UnsafePath(name.to_owned()));
            }
        }
    }
    Ok(out)
}

/// Whether the parts `parts` (already free of `..`) are a name a backup may hold.
fn allowed(name: &str, parts: &[String]) -> Result<(), Refusal> {
    let top = parts.first().map_or("", String::as_str);
    let fine = if top == BACKUP_SETTINGS {
        true
    } else {
        FILES.contains(&top) && parts.len() == 1
    };
    if fine {
        Ok(())
    } else {
        Err(Refusal::UnknownEntry(name.to_owned()))
    }
}

fn unpack(source: impl Read, dir: &Path, limit: u64) -> Result<(), RestoreError> {
    let mut reader = BufReader::new(source);
    let head = reader.fill_buf().map_err(io_error("cannot read", dir))?;
    if head.get(..2) != Some(&[0x1f, 0x8b]) {
        return Err(Refusal::NotGzip.into());
    }
    let damaged = |e: io::Error| RestoreError::from(Refusal::Damaged(e.to_string()));
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let mut total = 0_u64;
    for entry in archive.entries().map_err(damaged)? {
        let mut entry = entry.map_err(damaged)?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let parts = components(&name)?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            return Err(Refusal::Link(name).into());
        }
        if parts.is_empty() {
            continue;
        }
        allowed(&name, &parts)?;
        let target = parts.iter().fold(dir.to_path_buf(), |p, part| p.join(part));
        if kind.is_dir() {
            private::create_dir(&target).map_err(io_error("cannot create", &target))?;
        } else if kind.is_file() {
            total = total.saturating_add(entry.size());
            if total > limit {
                return Err(Refusal::TooLarge.into());
            }
            if let Some(parent) = target.parent() {
                private::create_dir(parent).map_err(io_error("cannot create", parent))?;
            }
            let mut out = create_part(&target, &name)?;
            io::copy(&mut entry, &mut out).map_err(damaged)?;
        } else {
            return Err(Refusal::UnknownEntry(name).into());
        }
    }
    Ok(())
}

/// Creates the file `target` for the archive entry `name`; the same name twice is a refusal.
fn create_part(target: &Path, name: &str) -> Result<File, RestoreError> {
    match private::create_file(target) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            Err(Refusal::Duplicate(name.to_owned()).into())
        }
        Err(e) => Err(io_error("cannot create", target)(e)),
    }
}

fn copy_folder(folder: &Path, dir: &Path, limit: u64) -> Result<(), RestoreError> {
    let mut total = 0_u64;
    copy_below(folder, Path::new(""), dir, &mut total, limit)
}

fn copy_below(
    root: &Path,
    relative: &Path,
    dir: &Path,
    total: &mut u64,
    limit: u64,
) -> Result<(), RestoreError> {
    let here = root.join(relative);
    let mut entries = fs::read_dir(&here)
        .map_err(io_error("cannot read", &here))?
        .collect::<io::Result<Vec<_>>>()
        .map_err(io_error("cannot read", &here))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let rel = relative.join(entry.file_name());
        let name = rel.to_string_lossy().replace('\\', "/");
        let parts = components(&name)?;
        let kind = entry
            .file_type()
            .map_err(io_error("cannot read", &entry.path()))?;
        if kind.is_symlink() {
            return Err(Refusal::Link(name).into());
        }
        allowed(&name, &parts)?;
        let target = dir.join(&rel);
        if kind.is_dir() {
            private::create_dir(&target).map_err(io_error("cannot create", &target))?;
            copy_below(root, &rel, dir, total, limit)?;
        } else if kind.is_file() {
            let size = entry
                .metadata()
                .map_err(io_error("cannot read", &entry.path()))?
                .len();
            *total = total.saturating_add(size);
            if *total > limit {
                return Err(Refusal::TooLarge.into());
            }
            let mut from =
                File::open(entry.path()).map_err(io_error("cannot read", &entry.path()))?;
            let mut out =
                private::create_file(&target).map_err(io_error("cannot create", &target))?;
            io::copy(&mut from, &mut out).map_err(io_error("cannot copy", &entry.path()))?;
        } else {
            return Err(Refusal::UnknownEntry(name).into());
        }
    }
    Ok(())
}

/// How long a staged copy that nobody applied stays.
pub(crate) const STAGING_LIFETIME: std::time::Duration = std::time::Duration::from_hours(24);

/// Removes the staging folders below `root` that are older than `max_age`; the number removed. A
/// missing root has none. Folders are judged by the time they were made.
pub(crate) fn sweep_staging(root: &Path, max_age: std::time::Duration) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|made| now.duration_since(made).ok())
            .is_some_and(|age| age >= max_age);
        if old && fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

fn part(name: RestorePartName, counts: puddle_store::PartCounts) -> RestorePart {
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    RestorePart {
        name,
        in_backup: n(counts.in_backup),
        add: n(counts.add),
        differ: n(counts.differ),
        same: n(counts.same),
        only_here: n(counts.only_here),
    }
}

/// `workspaces.json` as id to name; none when the file is missing or not a list.
fn workspace_names(path: &Path) -> Vec<(String, String)> {
    let Ok(body) = fs::read(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = value
        .get("workspaces")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let id = w.get("id")?.as_str()?.to_owned();
            let name = w
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or(&id)
                .to_owned();
            Some((id, name))
        })
        .collect();
    out.sort();
    out
}

/// The settings documents below `dir` as `file#key` to the key's value, so a setting is compared by
/// its key (a file that is not a JSON object is one entry, by its text).
fn settings_entries(dir: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    walk_settings(dir, Path::new(""), &mut out);
    out
}

fn walk_settings(root: &Path, rel: &Path, out: &mut std::collections::BTreeMap<String, String>) {
    let Ok(entries) = fs::read_dir(root.join(rel)) else {
        return;
    };
    for entry in entries.flatten() {
        let rel = rel.join(entry.file_name());
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk_settings(root, &rel, out),
            Ok(kind) if kind.is_file() && !merge::is_temp(&rel) => {
                add_document(&rel, &entry.path(), out);
            }
            _ => {}
        }
    }
}

/// Adds the keys of the JSON document `path` (named `rel`), or the whole text of one that is not an
/// object, to `out`.
fn add_document(rel: &Path, path: &Path, out: &mut std::collections::BTreeMap<String, String>) {
    let name = rel.to_string_lossy().replace('\\', "/");
    let body = fs::read(path).unwrap_or_default();
    match serde_json::from_slice::<serde_json::Value>(&body) {
        Ok(serde_json::Value::Object(map)) => {
            for (key, value) in map.into_iter().filter(|(k, _)| k != "version") {
                out.insert(format!("{name}#{key}"), value.to_string());
            }
        }
        _ => {
            out.insert(name, String::from_utf8_lossy(&body).into_owned());
        }
    }
}

fn compare_maps(
    here: &std::collections::BTreeMap<String, String>,
    there: &std::collections::BTreeMap<String, String>,
) -> puddle_store::PartCounts {
    let mut counts = puddle_store::PartCounts {
        in_backup: there.len(),
        ..puddle_store::PartCounts::default()
    };
    for (key, value) in there {
        match here.get(key) {
            None => counts.add += 1,
            Some(v) if v == value => counts.same += 1,
            Some(_) => counts.differ += 1,
        }
    }
    counts.only_here = here.keys().filter(|k| !there.contains_key(*k)).count();
    counts
}

/// What restoring `staged` into the data folder `paths` names would do, for the preview. Reads the
/// present files and changes nothing.
///
/// # Errors
///
/// [`RestoreError::Store`] when either database cannot be read.
pub(crate) fn facts(paths: &HostPaths, staged: &Staged) -> Result<RestoreFacts, RestoreError> {
    let db = puddle_store::preview_merge(&paths.store(), &staged.dir.join(BACKUP_DATABASE))?;
    let there = workspace_names(&staged.dir.join(BACKUP_WORKSPACES));
    let here = workspace_names(&paths.workspace_book());
    let to_map = |v: &[(String, String)]| v.iter().cloned().collect();
    let settings = compare_maps(
        &settings_entries(&paths.settings()),
        &settings_entries(&staged.dir.join(BACKUP_SETTINGS)),
    );
    Ok(RestoreFacts {
        staging_id: staged.id.clone(),
        format: BACKUP_FORMAT,
        puddle_version: staged.manifest.as_ref().map(|m| m.puddle_version.clone()),
        schema_version: staged.schema_version,
        created_at_ms: staged.manifest.as_ref().map(|m| m.created_at_ms),
        parts: vec![
            part(RestorePartName::Rules, db.rules),
            part(RestorePartName::RuleSets, db.rule_sets),
            part(
                RestorePartName::Workspaces,
                compare_maps(&to_map(&here), &to_map(&there)),
            ),
            part(RestorePartName::Identities, db.identities),
            part(RestorePartName::Environment, db.environment),
            part(RestorePartName::Settings, settings),
            part(RestorePartName::Users, db.users),
        ],
        workspaces: there,
        secret_refs: db
            .secret_envs
            .into_iter()
            .map(|s| SecretRef {
                workspace_id: s.workspace_id,
                name: s.name,
                secret_id: s.secret_id,
            })
            .collect(),
    })
}

/// Reads and applies restores for one data folder: [`puddle_api::RestoreService`] over
/// [`stage_archive`], [`facts`], [`apply::request`] and the safety copies. The upload is spooled to
/// an unnamed temporary file first (its size does not set a memory bound), and the staging, the
/// database reads and the copies run on a blocking thread.
#[derive(Debug, Clone)]
pub(crate) struct DataFolderRestore {
    paths: HostPaths,
    /// Told when a restore is confirmed, so the process can stop and start again to apply it.
    requested: std::sync::Arc<tokio::sync::Notify>,
}

impl DataFolderRestore {
    pub(crate) fn new(paths: HostPaths, requested: std::sync::Arc<tokio::sync::Notify>) -> Self {
        Self { paths, requested }
    }
}

/// Runs `job` on a blocking thread; a job that panics is a failed request that says so.
async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, RestoreServiceError> + Send + 'static,
) -> Result<T, RestoreServiceError> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| RestoreServiceError::Failed(format!("the restore stopped: {e}")))?
}

/// A refusal is the user's to read; anything else is a failure of this machine.
fn refused_or_failed(error: RestoreError) -> RestoreServiceError {
    match error {
        RestoreError::Refused(why) => RestoreServiceError::Refused(why.to_string()),
        other => RestoreServiceError::Failed(other.to_string()),
    }
}

fn mode_of(mode: puddle_api::wire::RestoreMode) -> ApplyMode {
    match mode {
        puddle_api::wire::RestoreMode::Add => ApplyMode::Add,
        puddle_api::wire::RestoreMode::Replace => ApplyMode::Replace,
    }
}

/// What the preview counted, as one sentence for the message after the restart.
fn summarise(mode: ApplyMode, parts: &[RestorePart]) -> String {
    let total = |f: fn(&RestorePart) -> u32| parts.iter().map(f).sum::<u32>();
    let (add, differ, only_here) = (
        total(|p| p.add),
        total(|p| p.differ),
        total(|p| p.only_here),
    );
    match mode {
        ApplyMode::Add => format!(
            "Added {add} entries that were missing; kept your version of {differ} that differ."
        ),
        ApplyMode::Replace => format!(
            "Added {add}, replaced {differ} and dropped {only_here} entries that were not in the backup."
        ),
    }
}

/// A safety copy's name: the UTC time it was made.
fn is_safety_name(name: &str) -> bool {
    name.len() == 16
        && name.bytes().enumerate().all(|(i, b)| match i {
            8 => b == b'T',
            15 => b == b'Z',
            _ => b.is_ascii_digit(),
        })
}

/// Removes the safety copy folder `dir`.
fn remove_copy(dir: &Path) -> Result<(), RestoreServiceError> {
    fs::remove_dir_all(dir)
        .map_err(|e| RestoreServiceError::Failed(format!("cannot delete {}: {e}", dir.display())))
}

/// The size in bytes of the files below `dir`.
fn size_of(dir: &Path) -> u64 {
    fs::read_dir(dir).map_or(0, |entries| entries.flatten().map(|e| entry_size(&e)).sum())
}

fn entry_size(entry: &fs::DirEntry) -> u64 {
    let size = |m: fs::Metadata| {
        if m.is_dir() {
            size_of(&entry.path())
        } else {
            m.len()
        }
    };
    entry.metadata().map_or(0, size)
}

impl puddle_api::RestoreService for DataFolderRestore {
    fn preview(
        &self,
        upload: puddle_api::UploadStream,
    ) -> futures_util::future::BoxFuture<'_, Result<RestoreFacts, RestoreServiceError>> {
        let paths = self.paths.clone();
        Box::pin(async move {
            let spooled = spool(upload, &tempfile::env::temp_dir()).await?;
            blocking(move || {
                let root = paths.restore_staging();
                sweep_staging(&root, STAGING_LIFETIME);
                let staged = stage_archive(spooled, &root).map_err(refused_or_failed)?;
                facts(&paths, &staged)
                    .inspect_err(|_| discard(&staged.dir))
                    .map_err(refused_or_failed)
            })
            .await
        })
    }

    fn apply<'a>(
        &'a self,
        staging_id: &'a str,
        mode: puddle_api::wire::RestoreMode,
    ) -> futures_util::future::BoxFuture<
        'a,
        Result<puddle_api::wire::RestoreApplied, RestoreServiceError>,
    > {
        let paths = self.paths.clone();
        let id = staging_id.to_owned();
        Box::pin(async move {
            let mode = mode_of(mode);
            let requested = blocking(move || {
                let summary = |dir: &Path| {
                    let manifest = fs::read(dir.join(BACKUP_MANIFEST))
                        .ok()
                        .and_then(|b| serde_json::from_slice::<Manifest>(&b).ok());
                    let staged = Staged {
                        id: id.clone(),
                        dir: dir.to_path_buf(),
                        manifest,
                        schema_version: puddle_store::SCHEMA_VERSION,
                    };
                    facts(&paths, &staged)
                        .map(|f| summarise(mode, &f.parts))
                        .unwrap_or_default()
                };
                apply::request_with(&paths, &id, mode, summary).map_err(|e| match e {
                    apply::RequestError::NotStaged => RestoreServiceError::NotStaged(e.to_string()),
                    apply::RequestError::AlreadyPending | apply::RequestError::Refused(_) => {
                        RestoreServiceError::Refused(e.to_string())
                    }
                    apply::RequestError::Failed(_) => RestoreServiceError::Failed(e.to_string()),
                })
            })
            .await?;
            self.requested.notify_one();
            Ok(puddle_api::wire::RestoreApplied {
                safety_copy: requested.safety_copy,
                restart_required: true,
            })
        })
    }

    fn result(
        &self,
    ) -> futures_util::future::BoxFuture<
        '_,
        Result<Option<puddle_api::wire::RestoreResult>, RestoreServiceError>,
    > {
        let path = self.paths.restore_result();
        Box::pin(async move {
            blocking(move || match fs::read(&path) {
                Ok(body) => {
                    let outcome: apply::RestoreOutcome =
                        serde_json::from_slice(&body).map_err(|e| {
                            RestoreServiceError::Failed(format!(
                                "cannot read the last restore's result: {e}"
                            ))
                        })?;
                    Ok(Some(puddle_api::wire::RestoreResult {
                        applied: outcome.applied,
                        mode: match outcome.mode {
                            ApplyMode::Add => puddle_api::wire::RestoreMode::Add,
                            ApplyMode::Replace => puddle_api::wire::RestoreMode::Replace,
                        },
                        safety_copy: outcome.safety_copy,
                        at_ms: outcome.at_ms,
                        message: outcome.message,
                    }))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(RestoreServiceError::Failed(format!(
                    "cannot read the last restore's result: {e}"
                ))),
            })
            .await
        })
    }

    fn dismiss_result(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<(), RestoreServiceError>> {
        let path = self.paths.restore_result();
        Box::pin(async move {
            blocking(move || match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(RestoreServiceError::Failed(format!(
                    "cannot remove {}: {e}",
                    path.display()
                ))),
            })
            .await
        })
    }

    fn safety_copies(
        &self,
    ) -> futures_util::future::BoxFuture<
        '_,
        Result<Vec<puddle_api::wire::SafetyCopy>, RestoreServiceError>,
    > {
        let root = self.paths.safety_copies();
        Box::pin(async move {
            blocking(move || {
                let mut copies: Vec<puddle_api::wire::SafetyCopy> = match fs::read_dir(&root) {
                    Ok(entries) => entries
                        .flatten()
                        .filter_map(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            (is_safety_name(&name) && e.path().is_dir()).then(|| {
                                puddle_api::wire::SafetyCopy {
                                    size_bytes: size_of(&e.path()),
                                    name,
                                }
                            })
                        })
                        .collect(),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
                    Err(e) => {
                        return Err(RestoreServiceError::Failed(format!(
                            "cannot read {}: {e}",
                            root.display()
                        )));
                    }
                };
                copies.sort_by(|a, b| b.name.cmp(&a.name));
                Ok(copies)
            })
            .await
        })
    }

    fn delete_safety_copy<'a>(
        &'a self,
        name: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), RestoreServiceError>> {
        let dir = is_safety_name(name).then(|| self.paths.safety_copies().join(name));
        let name = name.to_owned();
        Box::pin(async move {
            blocking(move || match dir {
                Some(dir) if dir.is_dir() => remove_copy(&dir),
                _ => Err(RestoreServiceError::NotStaged(format!(
                    "no safety copy named {name}"
                ))),
            })
            .await
        })
    }
}

/// Writes the upload into an unnamed temporary file made in `scratch` and returns it at its first
/// byte; the file is gone when the last handle closes.
async fn spool(
    upload: puddle_api::UploadStream,
    scratch: &Path,
) -> Result<File, RestoreServiceError> {
    use futures_util::StreamExt as _;
    use tokio::io::AsyncWriteExt as _;
    let failed =
        |what: &str, e: &dyn std::fmt::Display| RestoreServiceError::Failed(format!("{what}: {e}"));
    let file = tempfile::tempfile_in(scratch)
        .map_err(|e| failed("cannot make a temporary file for the upload", &e))?;
    let mut out = tokio::fs::File::from_std(file);
    let mut upload = upload;
    while let Some(piece) = upload.next().await {
        let piece = piece.map_err(|e| failed("the upload stopped", &e))?;
        out.write_all(&piece)
            .await
            .map_err(|e| failed("cannot write the upload", &e))?;
    }
    out.flush()
        .await
        .map_err(|e| failed("cannot write the upload", &e))?;
    let mut file = out.into_std().await;
    io::Seek::rewind(&mut file).map_err(|e| failed("cannot read the upload back", &e))?;
    Ok(file)
}

pub(crate) mod apply;
mod merge;

#[cfg(test)]
mod tests;
