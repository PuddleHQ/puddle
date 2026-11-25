// SPDX-License-Identifier: AGPL-3.0-or-later
//! The file parts of a restore: the settings documents and the workspace list, merged or replaced
//! into a new file or folder next to the live one (BK-14, BK-15). The database is
//! `puddle_store::restore_database`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use puddle_fs::private;
use puddle_store::RestoredDatabase;
use serde_json::{Map, Value};

use super::{RestoreError, apply::ApplyMode};

/// The workspace list's current format.
const BOOK_VERSION: u64 = 4;

fn io_error(what: &str, path: &Path) -> impl FnOnce(io::Error) -> RestoreError {
    let what = format!("{what} {}", path.display());
    move |source| RestoreError::Io { what, source }
}

pub(super) fn is_temp(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("tmp"))
}

/// Copies the regular files below `from` to `to`, owner-only, leaving out temporary files and
/// links; no folder is no files.
fn copy_tree(from: &Path, to: &Path) -> Result<(), RestoreError> {
    let Ok(entries) = fs::read_dir(from) else {
        return Ok(());
    };
    private::create_dir(to).map_err(io_error("cannot create", to))?;
    for entry in entries {
        let entry = entry.map_err(io_error("cannot read", from))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry
            .file_type()
            .map_err(io_error("cannot read", &source))?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_file() && !is_temp(&source) {
            let body = fs::read(&source).map_err(io_error("cannot read", &source))?;
            private::write_atomic(&target, &body).map_err(io_error("cannot write", &target))?;
        }
    }
    Ok(())
}

/// Builds `new` from the present settings `here` and the backup's `there`. Add keeps every present
/// document and setting and adds what is missing (a setting by its key in a document: a key that is
/// set here wins); Replace takes the backup's documents.
pub(super) fn settings(
    mode: ApplyMode,
    here: &Path,
    there: &Path,
    new: &Path,
) -> Result<(), RestoreError> {
    match mode {
        ApplyMode::Replace => copy_tree(there, new)?,
        ApplyMode::Add => {
            copy_tree(here, new)?;
            add_missing(there, Path::new(""), new)?;
        }
    }
    keep_first_run(here, new)
}

/// Whether the first-run flow has been through is a fact about this machine, so a backup never
/// takes it over (BK-31): the new global document gets this machine's `first_run` value, or none.
fn keep_first_run(here: &Path, new: &Path) -> Result<(), RestoreError> {
    let target = new.join("global.json");
    let Ok(body) = fs::read(&target) else {
        return Ok(());
    };
    let Ok(Value::Object(mut doc)) = serde_json::from_slice::<Value>(&body) else {
        return Ok(());
    };
    let present = fs::read(here.join("global.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("first_run").cloned());
    if doc.get("first_run") == present.as_ref() {
        return Ok(());
    }
    match present {
        Some(value) => doc.insert("first_run".into(), value),
        None => doc.remove("first_run"),
    };
    let body = serde_json::to_vec_pretty(&Value::Object(doc)).map_err(|e| RestoreError::Io {
        what: format!("cannot write {}", target.display()),
        source: io::Error::other(e),
    })?;
    private::write_atomic(&target, &body).map_err(io_error("cannot write", &target))
}

fn add_missing(there: &Path, rel: &Path, new: &Path) -> Result<(), RestoreError> {
    let Ok(entries) = fs::read_dir(there.join(rel)) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(io_error("cannot read", there))?;
        let rel = rel.join(entry.file_name());
        let kind = entry
            .file_type()
            .map_err(io_error("cannot read", &entry.path()))?;
        let target = new.join(&rel);
        if kind.is_dir() {
            private::create_dir(&target).map_err(io_error("cannot create", &target))?;
            add_missing(there, &rel, new)?;
        } else if kind.is_file() && !is_temp(&entry.path()) {
            let theirs = fs::read(entry.path()).map_err(io_error("cannot read", &entry.path()))?;
            let parent = target.parent().unwrap_or(new);
            private::create_dir(parent).map_err(io_error("cannot create", parent))?;
            match fs::read(&target) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    private::write_atomic(&target, &theirs)
                        .map_err(io_error("cannot write", &target))?;
                }
                Err(e) => return Err(io_error("cannot read", &target)(e)),
                Ok(ours) => {
                    if let Some(merged) = merge_documents(&ours, &theirs) {
                        private::write_atomic(&target, &merged)
                            .map_err(io_error("cannot write", &target))?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The present document with the backup's keys it lacks added; `None` when nothing is added or
/// either is not a JSON object.
fn merge_documents(ours: &[u8], theirs: &[u8]) -> Option<Vec<u8>> {
    let (Value::Object(mut mine), Value::Object(other)) = (
        serde_json::from_slice::<Value>(ours).ok()?,
        serde_json::from_slice::<Value>(theirs).ok()?,
    ) else {
        return None;
    };
    let mut added = false;
    for (key, value) in other {
        if key != "version" && !mine.contains_key(&key) {
            mine.insert(key, value);
            added = true;
        }
    }
    added
        .then(|| serde_json::to_vec_pretty(&Value::Object(mine)).ok())
        .flatten()
}

// string-error-ok: the text is the refusal shown to the user before a restore starts; no caller branches on a kind
fn read_book(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let body = match fs::read(path) {
        Ok(body) => body,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(map)) => Ok(Some(map)),
        _ => Err(format!("{} is not a workspace list", path.display())),
    }
}

fn version_of(book: &Map<String, Value>) -> u64 {
    book.get("version").and_then(Value::as_u64).unwrap_or(0)
}

fn entries_of(book: &Map<String, Value>) -> Vec<Value> {
    book.get("workspaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The oldest workspace list format whose entries are the current ones without an owner: formats
/// 3 and 4 differ only by that field, so two lists in them merge. Formats 1 and 2 name and shape
/// their entries differently, and guessing the translation could lose a workspace's settings.
const OLDEST_MERGEABLE: u64 = 3;

/// Whether the present and the backup's workspace lists can be merged: one of them has no
/// workspaces, or both are in a format whose entries have the current shape (3 or 4).
// string-error-ok: the text is the refusal shown to the user before a restore starts; no caller branches on a kind
pub(super) fn check_book_versions(here: &Path, there: &Path) -> Result<(), String> {
    let (Some(here), Some(there)) = (read_book(here)?, read_book(there)?) else {
        return Ok(());
    };
    let (ours, theirs) = (version_of(&here), version_of(&there));
    if entries_of(&here).is_empty()
        || entries_of(&there).is_empty()
        || ours == theirs
        || (ours >= OLDEST_MERGEABLE && theirs >= OLDEST_MERGEABLE)
    {
        return Ok(());
    }
    let old = if theirs < OLDEST_MERGEABLE {
        theirs
    } else {
        ours
    };
    let whose = if theirs < OLDEST_MERGEABLE {
        "the backup's"
    } else {
        "this machine's"
    };
    Err(format!(
        "the workspace list in the backup is in format {theirs} and this machine's in format \
         {ours}. Puddle can merge formats {OLDEST_MERGEABLE} and {BOOK_VERSION}, but {whose} \
         format {old} describes workspaces differently, so adding would risk losing their \
         settings. Start the Puddle that made the backup once and back up again, choose Replace, \
         or restore on a machine with no workspaces"
    ))
}

/// The entries of a list with the owner `owner` given to each that names none.
fn with_owners(entries: Vec<Value>, owner: i64) -> Vec<Value> {
    entries
        .into_iter()
        .map(|mut entry| {
            if let Some(object) = entry.as_object_mut() {
                object.entry("owner").or_insert_with(|| Value::from(owner));
            }
            entry
        })
        .collect()
}

fn owner_of(entry: &Value, restored: &RestoredDatabase) -> i64 {
    entry
        .get("owner")
        .and_then(Value::as_i64)
        .and_then(|b| {
            restored
                .user_map
                .iter()
                .find(|(from, _)| *from == b)
                .map(|&(_, to)| to)
        })
        .unwrap_or(restored.owner_id)
}

/// The backup's entries with their owners as this machine's users have them (a version 4 list
/// names an owner; an older one names none and is read with the owners the database records).
fn restored_entries(book: &Map<String, Value>, restored: &RestoredDatabase) -> Vec<Value> {
    let current = version_of(book) == BOOK_VERSION;
    entries_of(book)
        .into_iter()
        .map(|mut entry| {
            if current && let Some(object) = entry.as_object_mut() {
                let owner = owner_of(&Value::Object(object.clone()), restored);
                object.insert("owner".into(), Value::from(owner));
            }
            entry
        })
        .collect()
}

fn ids_of(book: &Map<String, Value>, key: &str) -> Vec<Value> {
    book.get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn id_of(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

/// Writes the restored workspace list to `new`: Add keeps every present workspace and adds the
/// backup's that are missing by id; Replace takes the backup's list. Ids retired on either side
/// stay retired. A backup with no list leaves the present one alone (nothing is written).
pub(super) fn book(
    mode: ApplyMode,
    here: &Path,
    there: &Path,
    new: &Path,
    restored: &RestoredDatabase,
) -> Result<(), RestoreError> {
    let merge = RestoreError::Merge;
    let Some(theirs) = read_book(there).map_err(merge)? else {
        return Ok(());
    };
    let ours = read_book(here).map_err(RestoreError::Merge)?;
    let mut out = theirs.clone();
    let mut workspaces = restored_entries(&theirs, restored);
    if let Some(ours) = &ours {
        if mode == ApplyMode::Add && !entries_of(ours).is_empty() && !workspaces.is_empty() {
            check_book_versions(here, there).map_err(RestoreError::Merge)?;
            // A list in format 3 beside one in format 4: the merged list is format 4, and the
            // entries that name no owner get this machine's, as the loader gives them.
            let mixed = version_of(ours) != version_of(&theirs);
            let mut all = entries_of(ours);
            let present: BTreeSet<String> = all
                .iter()
                .filter_map(|e| id_of(e).map(str::to_owned))
                .collect();
            all.extend(
                workspaces
                    .into_iter()
                    .filter(|e| id_of(e).is_none_or(|id| !present.contains(id))),
            );
            workspaces = all;
            out.clone_from(ours);
            if mixed {
                workspaces = with_owners(workspaces, restored.owner_id);
                out.insert("version".into(), Value::from(BOOK_VERSION));
            }
        }
        // An id retired on either side is never given out again.
        let mut retired = ids_of(ours, "retired_ids");
        for id in ids_of(&theirs, "retired_ids") {
            if !retired.contains(&id) {
                retired.push(id);
            }
        }
        if !retired.is_empty() {
            out.insert("retired_ids".into(), Value::Array(retired));
        }
        if mode == ApplyMode::Add && out.get("cleaned_ids").is_none() {
            // Only the present side's cleaned ids are known to be true here.
            out.remove("cleaned_ids");
        }
    }
    out.insert("workspaces".into(), Value::Array(workspaces));
    // A JSON value always serialises.
    let body = serde_json::to_vec_pretty(&Value::Object(out)).unwrap_or_default();
    private::write_atomic(new, &body).map_err(io_error("cannot write", new))
}

#[cfg(test)]
mod tests;
