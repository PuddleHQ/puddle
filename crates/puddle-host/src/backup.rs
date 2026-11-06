// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle backup`: a consistent copy of the state a restore needs, made while the host runs.
//!
//! The copy holds the database (`puddle.db`: rules, pending requests, audit log, environment
//! rows, git identities), the settings documents (`settings/`) and the workspace list
//! (`workspaces.json`). It never holds what only makes sense on this machine or is secret: the
//! credential store's contents live in the operating system and are not read; the login markers
//! and the browser front's key halves, the connection file and the lock files stay behind; and
//! the sandbox runtime and the workspace volumes are too large and have their own owner.
//!
//! This process never takes the data-folder lock, since the host that runs holds it, and it
//! writes nothing in the data folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use puddle_fs::private;

use crate::HostPaths;

/// The database's file name inside a backup.
pub const BACKUP_DATABASE: &str = "puddle.db";
/// The settings folder's name inside a backup.
pub const BACKUP_SETTINGS: &str = "settings";
/// The workspace list's file name inside a backup.
pub const BACKUP_WORKSPACES: &str = "workspaces.json";

/// What a finished backup holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupReport {
    /// The folder the backup is in.
    pub dest: PathBuf,
    /// The database's schema version; restore it with a Puddle that knows it.
    pub schema_version: u32,
    /// How many settings files were copied.
    pub settings_files: usize,
    /// Whether the data folder had a workspace list to copy.
    pub workspace_list: bool,
}

/// Why a backup was not made. Messages name paths, never contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackupError {
    /// The destination already exists; a backup never writes into a folder that is there.
    #[error("{0} already exists; choose a new folder")]
    DestinationExists(PathBuf),
    /// The destination is inside the data folder, which would make the backup part of the
    /// next one.
    #[error("{0} is inside Puddle's data folder; choose a folder outside it")]
    InsideData(PathBuf),
    /// There is no database to copy.
    #[error("no database at {0}; is this the right data folder?")]
    NoDatabase(PathBuf),
    /// The database could not be copied or failed its check.
    #[error("cannot copy the database: {0}")]
    Database(#[from] puddle_store::StoreError),
    /// A file or folder could not be read or written.
    #[error("{what}: {source}")]
    Io {
        /// What was being done, with the path.
        what: String,
        /// What went wrong.
        source: io::Error,
    },
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> BackupError {
    let what = format!("{what} {}", path.display());
    move |source| BackupError::Io { what, source }
}

/// Makes a backup of the data folder `paths` names in the new folder `dest`, which is created
/// owner-only (its parent is created if missing). On failure the folder is removed again, so a
/// folder that exists is a complete backup.
///
/// # Errors
///
/// [`BackupError`] as its variants say.
pub fn backup(paths: &HostPaths, dest: &Path) -> Result<BackupReport, BackupError> {
    let source_db = paths.store();
    if !source_db.is_file() {
        return Err(BackupError::NoDatabase(source_db));
    }
    if dest.exists() {
        return Err(BackupError::DestinationExists(dest.to_path_buf()));
    }
    refuse_inside(paths.data(), dest)?;
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        private::create_dir(parent).map_err(io_error("cannot create", parent))?;
    }
    private::create_new_dir(dest).map_err(io_error("cannot create", dest))?;
    match fill(paths, dest) {
        Ok(report) => Ok(report),
        Err(err) => {
            // The folder is ours (create_new_dir made it a moment ago), so it is safe to remove.
            if let Err(cleanup) = fs::remove_dir_all(dest) {
                tracing::warn!(path = %dest.display(), error = %cleanup, "could not remove the unfinished backup");
            }
            Err(err)
        }
    }
}

fn fill(paths: &HostPaths, dest: &Path) -> Result<BackupReport, BackupError> {
    let db = dest.join(BACKUP_DATABASE);
    let info = puddle_store::backup_database(&paths.store(), &db)?;
    // VACUUM INTO makes the file with the process's default mode; the folder is owner-only, and
    // the file is tightened too so a later move out of the folder keeps it private.
    private::tighten_file(&db).map_err(io_error("cannot restrict", &db))?;

    let settings_files = copy_tree(&paths.settings(), &dest.join(BACKUP_SETTINGS))?;
    let workspace_list = copy_file(&paths.workspace_book(), &dest.join(BACKUP_WORKSPACES))?;
    Ok(BackupReport {
        dest: dest.to_path_buf(),
        schema_version: info.schema_version,
        settings_files,
        workspace_list,
    })
}

/// Refuses a destination that resolves into the data folder.
fn refuse_inside(data: &Path, dest: &Path) -> Result<(), BackupError> {
    let Ok(data) = data.canonicalize() else {
        return Ok(());
    };
    // The destination does not exist yet: resolve its deepest existing ancestor.
    let mut probe = dest;
    let resolved = loop {
        if let Ok(real) = probe.canonicalize() {
            break real;
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return Ok(()),
        }
    };
    if resolved.starts_with(&data) {
        return Err(BackupError::InsideData(dest.to_path_buf()));
    }
    Ok(())
}

/// Copies one regular file; `false` when it does not exist. Puddle writes these files
/// atomically (a rename), so a copy sees the old or the new file, never half of one.
fn copy_file(from: &Path, to: &Path) -> Result<bool, BackupError> {
    let body = match fs::read(from) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(io_error("cannot read", from)(e)),
    };
    private::write_atomic(to, &body).map_err(io_error("cannot write", to))?;
    Ok(true)
}

/// Copies the regular files below `from` to `to`, owner-only, skipping links and the
/// temporary files of an atomic write in progress; the number copied. No folder is no files.
fn copy_tree(from: &Path, to: &Path) -> Result<usize, BackupError> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(io_error("cannot read", from)(e)),
    };
    private::create_dir(to).map_err(io_error("cannot create", to))?;
    let mut copied = 0;
    for entry in entries {
        let entry = entry.map_err(io_error("cannot read", from))?;
        let name = entry.file_name();
        if name.to_string_lossy().ends_with(".tmp") {
            continue;
        }
        let kind = entry
            .file_type()
            .map_err(io_error("cannot read", &entry.path()))?;
        if kind.is_dir() {
            copied += copy_tree(&entry.path(), &to.join(&name))?;
        } else if kind.is_file() && copy_file(&entry.path(), &to.join(&name))? {
            copied += 1;
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use puddle_store::{Actor, Effect, Limits, NewRule, Pattern, Scope, Store, SystemClock};

    use super::*;

    const CANARY: &str = "CANARY-backup-credential-value";

    fn open(path: &Path) -> Store {
        Store::open(path, Arc::new(SystemClock), Limits::default()).unwrap()
    }

    fn add(store: &Store, n: usize) {
        store
            .add_rule(&NewRule {
                scope: Scope::Global,
                pattern: Pattern::parse(&format!("host{n}.example.org")).unwrap(),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }

    /// A data folder as a host leaves it, with files that must stay out of a backup.
    fn data_folder(root: &Path) -> HostPaths {
        let paths = HostPaths::new(root.join("data"));
        fs::create_dir_all(paths.settings()).unwrap();
        fs::write(paths.settings().join("global.json"), br#"{"version":1}"#).unwrap();
        fs::create_dir_all(paths.settings().join("workspaces")).unwrap();
        fs::write(paths.settings().join("workspaces").join("a.json"), b"{}").unwrap();
        fs::write(paths.settings().join(".global.json.1234.tmp"), b"half").unwrap();
        fs::write(paths.workspace_book(), br#"{"version":2,"workspaces":[]}"#).unwrap();
        fs::create_dir_all(paths.login_markers()).unwrap();
        fs::write(paths.login_markers().join("a"), CANARY).unwrap();
        fs::create_dir_all(paths.front_keys()).unwrap();
        fs::write(paths.front_keys().join("a.key"), CANARY).unwrap();
        fs::write(paths.connection_file(), CANARY).unwrap();
        fs::write(paths.data().join("host.lock"), b"").unwrap();
        paths
    }

    fn files_below(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                out.extend(files_below(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn backup_restores_to_an_equal_store_and_leaves_credentials_out() {
        let root = tempfile::tempdir().unwrap();
        let paths = data_folder(root.path());
        let live = open(&paths.store());
        for n in 0..5 {
            add(&live, n);
        }
        let dest = root.path().join("out");
        let report = backup(&paths, &dest).unwrap();
        assert_eq!(
            report.settings_files, 2,
            "the half-written temp file stays out"
        );
        assert!(report.workspace_list);

        // Hostile case: nothing credential-shaped reaches the destination, by name or content.
        for file in files_below(&dest) {
            let body = fs::read(&file).unwrap();
            assert!(
                !String::from_utf8_lossy(&body).contains(CANARY),
                "{} holds the canary",
                file.display()
            );
        }
        let names: Vec<_> = files_below(&dest)
            .iter()
            .map(|p| p.strip_prefix(&dest).unwrap().to_path_buf())
            .collect();
        assert!(
            names.iter().all(|n| {
                let s = n.to_string_lossy();
                !s.contains("login-markers")
                    && !s.contains("front")
                    && !s.contains("api.json")
                    && !s.contains(".lock")
                    && !s.ends_with("-wal")
                    && !s.ends_with("-shm")
                    && !s.ends_with(".tmp")
            }),
            "{names:?}"
        );

        // Opening the copy makes its -wal/-shm, so it comes after the listing above.
        let restored = open(&dest.join(BACKUP_DATABASE));
        assert_eq!(restored.rules(), live.rules());
        assert_eq!(
            restored.audit_lines(0, 1000).unwrap(),
            live.audit_lines(0, 1000).unwrap()
        );
        assert_eq!(
            fs::read(dest.join(BACKUP_WORKSPACES)).unwrap(),
            fs::read(paths.workspace_book()).unwrap()
        );
    }

    #[test]
    fn backup_while_the_store_is_being_written_is_one_consistent_point() {
        let root = tempfile::tempdir().unwrap();
        let paths = data_folder(root.path());
        let live = Arc::new(open(&paths.store()));
        add(&live, 0);
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (live, stop) = (Arc::clone(&live), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut n = 1;
                while !stop.load(Ordering::Relaxed) {
                    add(&live, n);
                    n += 1;
                }
                n
            })
        };
        // Wait until writes are flowing, then copy repeatedly while they continue.
        puddle_test_kit::eventually_blocking(
            "the writer has committed 20 rules",
            std::time::Duration::from_secs(10),
            || live.rules().len() >= 20,
        );
        let mut copies = Vec::new();
        for i in 0..5 {
            let dest = root.path().join(format!("out{i}"));
            let before = live.rules().len();
            backup(&paths, &dest).unwrap();
            copies.push((dest, before));
        }
        stop.store(true, Ordering::Relaxed);
        let total = writer.join().unwrap();

        let final_rules = live.rules();
        assert_eq!(final_rules.len(), total);
        for (dest, before) in copies {
            let restored = open(&dest.join(BACKUP_DATABASE));
            let rules = restored.rules();
            // A prefix of the live history: every rule the host had committed before the copy
            // began is in it, and it matches the live store row for row up to its own length.
            assert!(rules.len() >= before, "{} < {before}", rules.len());
            assert!(rules.len() <= final_rules.len());
            let live_prefix: Vec<_> = final_rules.iter().take(rules.len()).cloned().collect();
            assert_eq!(rules, live_prefix);
            // The audit log is a prefix too, with no gap.
            let lines = restored.audit_lines(0, 100_000).unwrap();
            let live_lines = live.audit_lines(0, 100_000).unwrap();
            assert_eq!(lines[..], live_lines[..lines.len()]);
        }
    }

    #[test]
    fn existing_destination_is_refused_and_untouched() {
        let root = tempfile::tempdir().unwrap();
        let paths = data_folder(root.path());
        drop(open(&paths.store()));
        let dest = root.path().join("out");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("mine"), b"keep").unwrap();
        assert!(matches!(
            backup(&paths, &dest),
            Err(BackupError::DestinationExists(_))
        ));
        assert_eq!(fs::read(dest.join("mine")).unwrap(), b"keep");
    }

    #[test]
    fn destination_inside_the_data_folder_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let paths = data_folder(root.path());
        drop(open(&paths.store()));
        let dest = paths.data().join("settings").join("out");
        assert!(matches!(
            backup(&paths, &dest),
            Err(BackupError::InsideData(_))
        ));
        assert!(!dest.exists());
    }

    #[test]
    fn missing_database_is_refused_and_creates_nothing() {
        let root = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(root.path().join("data"));
        let dest = root.path().join("out");
        assert!(matches!(
            backup(&paths, &dest),
            Err(BackupError::NoDatabase(_))
        ));
        assert!(!dest.exists());
        assert!(!paths.data().exists(), "the data folder is not created");
    }

    #[test]
    fn failed_copy_leaves_no_folder_behind() {
        let root = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(root.path().join("data"));
        fs::create_dir_all(paths.data()).unwrap();
        fs::write(paths.store(), b"this is not a database").unwrap();
        let dest = root.path().join("out");
        assert!(matches!(
            backup(&paths, &dest),
            Err(BackupError::Database(_))
        ));
        assert!(!dest.exists());
    }

    #[cfg(unix)]
    #[test]
    fn backup_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let paths = data_folder(root.path());
        drop(open(&paths.store()));
        let dest = root.path().join("out");
        backup(&paths, &dest).unwrap();
        for path in files_below(&dest).into_iter().chain([dest.clone()]) {
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o077;
            assert_eq!(mode, 0, "{} is open to others", path.display());
        }
    }

    #[test]
    fn missing_settings_and_book_are_fine() {
        let root = tempfile::tempdir().unwrap();
        let paths = HostPaths::new(root.path().join("data"));
        fs::create_dir_all(paths.data()).unwrap();
        drop(open(&paths.store()));
        let report = backup(&paths, &root.path().join("out")).unwrap();
        assert_eq!(report.settings_files, 0);
        assert!(!report.workspace_list);
    }
}
