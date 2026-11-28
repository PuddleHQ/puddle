// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore, second half: the confirmed restore and its apply at the next start (backup page, BK-11,
//! BK-13 to BK-18).
//!
//! Confirming ([`request`]) makes the safety copy, moves the staged parts into `restore-pending/`
//! and writes a marker. The next start ([`apply_pending`]), before the store opens and while the host
//! alone holds the data-folder lock, builds the restored parts next to the live ones, then swaps them
//! in part by part. The marker records how far it got, and each step can be run again, so a crash
//! at any point is finished (or, when a step fails, undone) at the following start. The marker is
//! removed last.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use puddle_fs::private;
use puddle_store::RestoreMode;
use serde::{Deserialize, Serialize};

use super::{RestoreError, merge, staging_dir};
use crate::HostPaths;
use crate::backup::{BACKUP_DATABASE, BACKUP_SETTINGS, BACKUP_WORKSPACES, make_backup};

/// Serialises [`request_with`]: the safety copy takes seconds, and the pending folder is shared.
static REQUESTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

const MARKER: &str = "marker.json";
const STAGED: &str = "staged";
const NEW: &str = "new";
const OLD: &str = "old";
/// The parts a restore swaps, in swap order.
const PARTS: [&str; 3] = [BACKUP_DATABASE, BACKUP_SETTINGS, BACKUP_WORKSPACES];
/// What SQLite keeps beside the database file while it is open.
const SIDECARS: [&str; 2] = ["puddle.db-wal", "puddle.db-shm"];

/// Which way a restore treats what is already here (BK-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    /// "Add what is missing".
    Add,
    /// "Replace".
    Replace,
}

impl From<ApplyMode> for RestoreMode {
    fn from(mode: ApplyMode) -> Self {
        match mode {
            ApplyMode::Add => Self::Add,
            ApplyMode::Replace => Self::Replace,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    /// Confirmed; nothing changed yet.
    Requested,
    /// The restored parts are complete in `new/`.
    Prepared,
    /// Swapping `new/` into place.
    Swapping,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    format: u32,
    mode: ApplyMode,
    /// The folder name of the safety copy below `safety-copies/`.
    safety_copy: String,
    /// What the preview said would change, for the message after the restart.
    summary: String,
    requested_at_ms: u64,
    phase: Phase,
}

/// What a restore did, kept in `restore-result.json` until the user has seen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RestoreOutcome {
    /// Whether the restore was applied; `false` means it was undone and the old state is in use.
    pub applied: bool,
    /// The mode that was asked for.
    pub mode: ApplyMode,
    /// The safety copy's folder name below `safety-copies/`.
    pub safety_copy: String,
    /// When it finished, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// What changed, or why it was undone.
    pub message: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> RestoreError {
    let what = format!("{what} {}", path.display());
    move |source| RestoreError::Io { what, source }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), RestoreError> {
    // A struct of strings and numbers always serialises.
    let body = serde_json::to_vec_pretty(value).unwrap_or_default();
    private::write_atomic(path, &body).map_err(io_error("cannot write", path))
}

/// The UTC time as `20261115T225500Z`, the name of a safety copy.
fn utc_name(ms: u64) -> String {
    puddle_services::backup::archive_file_name(ms)
        .trim_start_matches("puddle-backup-")
        .trim_end_matches(".tar.gz")
        .to_owned()
}

/// What [`request`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Requested {
    /// The safety copy's folder name below `safety-copies/`.
    pub safety_copy: String,
}

/// Why a restore was not confirmed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum RequestError {
    /// The staging id names nothing (it was never made, or was removed).
    #[error("this backup is no longer staged; choose the file again")]
    NotStaged,
    /// Another restore waits for the next start.
    #[error("a restore is already waiting for the next start of Puddle")]
    AlreadyPending,
    /// The backup cannot be merged with this machine's data, and why.
    #[error("{0}")]
    Refused(String),
    /// The safety copy or the pending files could not be written; nothing was changed.
    #[error("{0}")]
    Failed(String),
}

/// Confirms the restore of the staged backup `staging_id` in `mode`: makes the safety copy of the
/// present state in `safety-copies/<UTC time>/`, moves the staged parts to `restore-pending/` and
/// writes the marker. Does not change a live file. When the safety copy cannot be made the restore
/// does not start.
///
/// `summary` is the preview's account of what will change, kept for the message after the restart.
///
/// # Errors
///
/// [`RequestError`] as its variants say.
#[cfg(test)]
pub(crate) fn request(
    paths: &HostPaths,
    staging_id: &str,
    mode: ApplyMode,
    summary: &str,
) -> Result<Requested, RequestError> {
    request_with(paths, staging_id, mode, |_| summary.to_owned())
}

/// [`request`] with the summary made from the staged folder once it is known to exist.
pub(crate) fn request_with(
    paths: &HostPaths,
    staging_id: &str,
    mode: ApplyMode,
    summary: impl FnOnce(&Path) -> String,
) -> Result<Requested, RequestError> {
    // One confirm at a time: a second one that ran beside the first would clear the first one's
    // `restore-pending/` after the first had answered "restart" (two clicks, a retry, two tabs).
    let _one_at_a_time = REQUESTING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let fail = |what: &str, e: &dyn std::fmt::Display| RequestError::Failed(format!("{what}: {e}"));
    let pending = paths.restore_pending();
    if pending.join(MARKER).exists() {
        return Err(RequestError::AlreadyPending);
    }
    let staged = staging_dir(&paths.restore_staging(), staging_id)
        .filter(|d| d.join(BACKUP_DATABASE).is_file())
        .ok_or(RequestError::NotStaged)?;
    book_versions_agree(paths, &staged).map_err(|why| RequestError::Refused(why.to_string()))?;

    let requested_at_ms = now_ms();
    let name = utc_name(requested_at_ms);
    let copy = paths.safety_copies().join(&name);
    make_backup(paths, &copy).map_err(|e| {
        RequestError::Failed(format!(
            "the safety copy of the present state could not be made, so the restore did not start: {e}"
        ))
    })?;

    // Read before the staged folder moves: afterwards `staged` names nothing.
    let summary = summary(&staged);

    // A leftover without a marker is an earlier request that never finished: it is ours to clear.
    drop(fs::remove_dir_all(&pending));
    private::create_dir(&pending).map_err(|e| fail("cannot create the pending folder", &e))?;
    fs::rename(&staged, pending.join(STAGED))
        .map_err(|e| fail("cannot move the staged backup", &e))?;
    write_json(
        &pending.join(MARKER),
        &Marker {
            format: 1,
            mode,
            safety_copy: name.clone(),
            summary,
            requested_at_ms,
            phase: Phase::Requested,
        },
    )
    .map_err(|e| fail("cannot write the restore marker", &e))?;
    Ok(Requested { safety_copy: name })
}

/// Runs the confirmed restore, if one waits: call it at start, before the store opens, with the
/// data-folder lock held. Clears the staging folder (an upload that was not confirmed does not
/// outlive a start) and returns what happened, or `None` when nothing was pending. The outcome is
/// also written to `restore-result.json`.
///
/// A step that fails is undone: the live parts come back and Puddle starts with the old state; the
/// outcome says why. A crash is finished at the next call from the marker.
pub(crate) fn apply_pending(paths: &HostPaths) -> Option<RestoreOutcome> {
    apply_pending_with(paths, run)
}

/// [`apply_pending`] with the run of the marker's steps given, so a test can fail them.
fn apply_pending_with(
    paths: &HostPaths,
    run: impl Fn(&HostPaths, &Path, Marker) -> Result<(), RestoreError>,
) -> Option<RestoreOutcome> {
    drop(fs::remove_dir_all(paths.restore_staging()));
    let pending = paths.restore_pending();
    let marker_path = pending.join(MARKER);
    let marker = match fs::read(&marker_path) {
        Ok(body) => serde_json::from_slice::<Marker>(&body).map_err(|e| e.to_string()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // No marker: a request that never got that far. Nothing was changed.
            drop(fs::remove_dir_all(&pending));
            return None;
        }
        Err(e) => Err(e.to_string()),
    };
    let marker = match marker {
        Ok(marker) => marker,
        Err(why) => {
            let outcome = RestoreOutcome {
                applied: false,
                mode: ApplyMode::Add,
                safety_copy: String::new(),
                at_ms: now_ms(),
                message: format!(
                    "the restore marker could not be read ({why}); the restore was not applied and the present data was kept"
                ),
            };
            // The unreadable marker stays in a folder of its own for a person to look at; when it
            // cannot be moved it is removed, so it cannot block every later restore.
            drop(write_json(&paths.restore_result(), &outcome));
            if park(paths, &pending, "restore-pending.unreadable").is_err() {
                drop(fs::remove_dir_all(&pending));
            }
            return Some(outcome);
        }
    };
    let ran = run(paths, &pending, marker.clone());
    if let Err(RestoreError::RollBack { .. }) = &ran {
        return Some(keep_after_failed_roll_back(paths, &pending, &marker, &ran));
    }
    let outcome = match ran {
        Ok(()) => RestoreOutcome {
            applied: true,
            mode: marker.mode,
            safety_copy: marker.safety_copy.clone(),
            at_ms: now_ms(),
            message: format!(
                "Restored from a backup ({}). {} Your previous state is in the safety copy {}.",
                match marker.mode {
                    ApplyMode::Add => "add what is missing",
                    ApplyMode::Replace => "replace",
                },
                marker.summary,
                marker.safety_copy
            ),
        },
        Err(why) => RestoreOutcome {
            applied: false,
            mode: marker.mode,
            safety_copy: marker.safety_copy.clone(),
            at_ms: now_ms(),
            message: format!(
                "The restore failed and was undone; Puddle started with the state it had before. Cause: {why}. The safety copy {} holds that state.",
                marker.safety_copy
            ),
        },
    };
    drop(write_json(&paths.restore_result(), &outcome));
    // The marker goes last: a crash before this line runs the (idempotent) steps again.
    drop(fs::remove_dir_all(pending.join(STAGED)));
    drop(fs::remove_dir_all(pending.join(NEW)));
    drop(fs::remove_dir_all(pending.join(OLD)));
    drop(fs::remove_file(&marker_path));
    drop(fs::remove_dir(&pending));
    Some(outcome)
}

/// A roll-back step failed, so `old/` holds files the live folder does not: nothing is deleted.
/// The whole pending folder (marker, `old/`, `new/`) moves to a folder of its own and the outcome
/// names it and the safety copy.
fn keep_after_failed_roll_back(
    paths: &HostPaths,
    pending: &Path,
    marker: &Marker,
    ran: &Result<(), RestoreError>,
) -> RestoreOutcome {
    let cause = ran
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    let kept = match park(paths, pending, "restore-pending.rollback-failed") {
        Ok(name) => format!("The files it moved aside are kept in {name}"),
        Err(e) => format!("The files it moved aside stay in restore-pending/old ({e})"),
    };
    let outcome = RestoreOutcome {
        applied: false,
        mode: marker.mode,
        safety_copy: marker.safety_copy.clone(),
        at_ms: now_ms(),
        message: format!(
            "The restore failed and could not be fully undone: Puddle started with a mix of restored and previous files. Cause: {cause}. {kept}, and the safety copy {} holds the previous state.",
            marker.safety_copy
        ),
    };
    drop(write_json(&paths.restore_result(), &outcome));
    outcome
}

/// Moves `from` to `<data folder>/<name>`, or to `<name>-<time>` when that name is taken (a rename
/// onto a non-empty folder fails). Returns the name used.
fn park(paths: &HostPaths, from: &Path, name: &str) -> io::Result<String> {
    let first = paths.data().join(name);
    if !first.exists() {
        fs::rename(from, &first)?;
        return Ok(name.to_owned());
    }
    let unique = format!("{name}-{}", now_ms());
    fs::rename(from, paths.data().join(&unique))?;
    Ok(unique)
}

fn run(paths: &HostPaths, pending: &Path, marker: Marker) -> Result<(), RestoreError> {
    run_with(paths, pending, marker, swap_part, roll_back_part)
}

/// [`run`] with the swap and the roll-back of one part given, so a test can fail them.
fn run_with(
    paths: &HostPaths,
    pending: &Path,
    mut marker: Marker,
    swap: impl Fn(&HostPaths, &Path, &Path, &str) -> Result<(), RestoreError>,
    roll_back: impl Fn(&HostPaths, &Path, &str) -> io::Result<()>,
) -> Result<(), RestoreError> {
    let marker_path = pending.join(MARKER);
    let (new, old) = (pending.join(NEW), pending.join(OLD));
    if marker.phase == Phase::Requested {
        // The host kept running between the confirm and this start: whatever changed since is not
        // in the confirm-time copy, so the copy is made again before anything is touched.
        refresh_safety_copy(paths, &marker.safety_copy)?;
        drop(fs::remove_dir_all(&new));
        if let Err(e) = prepare(paths, &pending.join(STAGED), &new, marker.mode) {
            drop(fs::remove_dir_all(&new));
            return Err(e);
        }
        marker.phase = Phase::Prepared;
        write_json(&marker_path, &marker)?;
    }
    marker.phase = Phase::Swapping;
    write_json(&marker_path, &marker)?;
    private::create_dir(&old).map_err(io_error("cannot create", &old))?;
    let swapped = PARTS
        .iter()
        .try_for_each(|part| swap(paths, &new, &old, part));
    if let Err(e) = swapped {
        // Put every part back that was moved aside, then report why. Every part is tried, and a
        // part that does not come back is named.
        let stuck: Vec<String> = PARTS
            .iter()
            .filter_map(|part| {
                roll_back(paths, &old, part)
                    .err()
                    .map(|e| format!("{part}: {e}"))
            })
            .collect();
        if !stuck.is_empty() {
            return Err(RestoreError::RollBack {
                cause: e.to_string(),
                stuck: stuck.join("; "),
            });
        }
        return Err(e);
    }
    Ok(())
}

/// Makes the safety copy `name` again from the live state, in place: the copy is built beside the
/// old one and renamed over it, so the name the user was told stays, and a failure (or a crash)
/// leaves the confirm-time copy. It runs while the host alone holds the data folder and the store
/// is closed, so nothing can change after it.
fn refresh_safety_copy(paths: &HostPaths, name: &str) -> Result<(), RestoreError> {
    if !paths.store().is_file() {
        // Nothing to copy: the database is gone since the confirm, and the earlier copy has it.
        return Ok(());
    }
    let copy = paths.safety_copies().join(name);
    let fresh = paths.safety_copies().join(format!("{name}.refresh"));
    let fail =
        |what: &str, e: &dyn std::fmt::Display| RestoreError::SafetyCopy(format!("{what}: {e}"));
    // A leftover of a crash is ours: it ends in `.refresh`, which no safety copy's name does.
    drop(fs::remove_dir_all(&fresh));
    make_backup(paths, &fresh).map_err(|e| fail("cannot make it", &e))?;
    // The user may have deleted the confirm-time copy since: then the fresh one takes its name.
    let gone = fs::remove_dir_all(&copy)
        .or_else(|e| (e.kind() == io::ErrorKind::NotFound).then_some(()).ok_or(e));
    gone.map_err(|e| fail("cannot replace the earlier copy", &e))?;
    fs::rename(&fresh, &copy).map_err(|e| fail("cannot put it in place", &e))
}

/// The live location of a part.
fn live(paths: &HostPaths, part: &str) -> PathBuf {
    paths.data().join(part)
}

fn files_of(part: &str) -> Vec<String> {
    if part == BACKUP_DATABASE {
        std::iter::once(part)
            .chain(SIDECARS)
            .map(str::to_owned)
            .collect()
    } else {
        vec![part.to_owned()]
    }
}

/// Moves `new/<part>` into place and the live part to `old/`. Each file moves only when its
/// source exists and its target does not, so running it again after a crash finishes the job.
fn swap_part(paths: &HostPaths, new: &Path, old: &Path, part: &str) -> Result<(), RestoreError> {
    if !new.join(part).exists() {
        // Nothing prepared for this part, or already swapped.
        return Ok(());
    }
    for name in files_of(part) {
        let (from, aside) = (live(paths, &name), old.join(&name));
        if from.exists() && !aside.exists() {
            fs::rename(&from, &aside).map_err(io_error("cannot move aside", &from))?;
        }
    }
    let (from, to) = (new.join(part), live(paths, part));
    fs::rename(&from, &to).map_err(io_error("cannot move into place", &to))
}

/// Undoes [`swap_part`]: the part that was moved aside goes back over whatever is live now.
fn roll_back_part(paths: &HostPaths, old: &Path, part: &str) -> io::Result<()> {
    for name in files_of(part) {
        let (aside, to) = (old.join(&name), live(paths, &name));
        if aside.exists() {
            if to.is_dir() {
                fs::remove_dir_all(&to)?;
            } else if to.exists() {
                fs::remove_file(&to)?;
            }
            fs::rename(&aside, &to)?;
        }
    }
    Ok(())
}

/// Builds the restored parts in `new/` from the live ones and the staged backup.
fn prepare(
    paths: &HostPaths,
    staged: &Path,
    new: &Path,
    mode: ApplyMode,
) -> Result<(), RestoreError> {
    private::create_dir(new).map_err(io_error("cannot create", new))?;
    let db = new.join(BACKUP_DATABASE);
    let restored = restored_database(paths, &staged.join(BACKUP_DATABASE), &db, mode)?;
    private::tighten_file(&db).map_err(io_error("cannot restrict", &db))?;
    let ours = Files::new(paths.data());
    let theirs = Files::new(staged);
    let to = Files::new(new);
    merge::settings(mode, &ours.settings, &theirs.settings, &to.settings)?;
    merge::book(mode, &ours.book, &theirs.book, &to.book, &restored)
}

/// Where the settings and the workspace list are below a folder.
struct Files {
    settings: PathBuf,
    book: PathBuf,
}

impl Files {
    fn new(dir: &Path) -> Self {
        Self {
            settings: dir.join(BACKUP_SETTINGS),
            book: dir.join(BACKUP_WORKSPACES),
        }
    }
}

/// Writes the restored database to `db`: the live one with the backup's merged in, or the backup's
/// as it is on a machine that has no database yet.
fn restored_database(
    paths: &HostPaths,
    theirs: &Path,
    db: &Path,
    mode: ApplyMode,
) -> Result<puddle_store::RestoredDatabase, RestoreError> {
    let ours = paths.store();
    if ours.is_file() {
        let restored = puddle_store::restore_database(&ours, theirs, db, mode.into())?;
        return Ok(restored);
    }
    fs::copy(theirs, db).map_err(io_error("cannot copy", db))?;
    Ok(puddle_store::RestoredDatabase {
        user_map: Vec::new(),
        owner_id: 1,
        dropped_rows: 0,
    })
}

/// Whether the two workspace lists can be merged, for the request to refuse early.
fn book_versions_agree(paths: &HostPaths, staged: &Path) -> Result<(), merge::BookRefusal> {
    merge::check_book_versions(&paths.workspace_book(), &staged.join(BACKUP_WORKSPACES))
}

#[cfg(test)]
mod tests;
