// SPDX-License-Identifier: AGPL-3.0-or-later
//! Moves every value from one secret store to the other, for `puddle secrets migrate`.
//!
//! The order is what makes a crash harmless. Every value is read from the old store first (a
//! store that cannot be read changes nothing), then written to the new one and read back and
//! compared, and only then does `switch` record the new store as the one in charge. A failure
//! before the switch removes the copies and leaves the old store in charge. After the switch the
//! old store's values are deleted; a crash there leaves them behind, and [`clear_old_store`]
//! removes them on a rerun. Values the new store holds after the switch are never copied back, so
//! the store in charge wins every disagreement.
//!
//! Nothing here returns, logs or formats an id or a value: results are counts.

use std::collections::HashSet;

use crate::choice::RecordError;
use crate::secret::Secret;
use crate::store::SecretStore;
use puddle_types::StoredId;

/// Why a move stopped. Whatever the cause, the old store is still in charge and holds every value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MigrateError {
    /// The old store could not be read.
    #[error("the old store could not be read, so nothing was moved")]
    Source,
    /// The new store refused a value, or gave back another one than it was given.
    #[error(
        "the new store refused a value or changed it, so nothing was moved and the old store is still in charge"
    )]
    Target,
    /// `secrets/store.json` could not be written.
    #[error(
        "`secrets/store.json` could not be written, so nothing was moved and the old store is still in charge"
    )]
    Record,
}

/// What a finished move did, as counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Moved {
    /// Values now in the new store.
    pub copied: usize,
    /// Values the old store would not give up. The new store is in charge anyway; a rerun
    /// removes them.
    pub left: usize,
}

/// What [`clear_old_store`] did, as counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cleared {
    /// Values removed from the old store.
    pub removed: usize,
    /// Values it would not give up, or could not be asked about.
    pub left: usize,
}

/// `ids` without repeats, in order.
fn unique(ids: &[StoredId]) -> Vec<&StoredId> {
    let mut seen = HashSet::new();
    ids.iter().filter(|id| seen.insert(*id)).collect()
}

/// Copies the values `old` holds under `ids` to `new`, checks the copies, calls `switch` and
/// deletes the originals. An id `old` holds no value under is skipped.
///
/// # Errors
/// [`MigrateError`] when the old store cannot be read, the new store refuses or changes a value,
/// or `switch` fails. The copies are removed again and `old` is untouched.
pub fn move_values(
    old: &dyn SecretStore,
    new: &dyn SecretStore,
    ids: &[StoredId],
    switch: &mut dyn FnMut() -> Result<(), RecordError>,
) -> Result<Moved, MigrateError> {
    let mut held: Vec<(&StoredId, Secret)> = Vec::new();
    for id in unique(ids) {
        if let Some(value) = old.get(id).map_err(|_| MigrateError::Source)? {
            held.push((id, value));
        }
    }
    let switched =
        copy_and_check(new, &held).and_then(|()| switch().map_err(|_| MigrateError::Record));
    if let Err(err) = switched {
        for (id, _) in &held {
            // The new store is not in charge, so a copy it cannot remove is only a leftover.
            let _ = new.delete(id);
        }
        return Err(err);
    }
    let left = held
        .iter()
        .filter(|(id, _)| old.delete(id).is_err())
        .count();
    Ok(Moved {
        copied: held.len(),
        left,
    })
}

fn copy_and_check(new: &dyn SecretStore, held: &[(&StoredId, Secret)]) -> Result<(), MigrateError> {
    for (id, value) in held {
        new.set(id, value).map_err(|_| MigrateError::Target)?;
    }
    for (id, value) in held {
        match new.get(id) {
            Ok(Some(copy)) if copy.expose() == value.expose() => {}
            _ => return Err(MigrateError::Target),
        }
    }
    Ok(())
}

/// Removes what `old` still holds under `ids`, after a move that crashed past its switch.
/// Nothing is copied: the store in charge keeps every value it has.
#[must_use]
pub fn clear_old_store(old: &dyn SecretStore, ids: &[StoredId]) -> Cleared {
    let mut cleared = Cleared::default();
    for id in unique(ids) {
        match old.get(id) {
            Ok(None) => {}
            Ok(Some(_)) if old.delete(id).is_ok() => cleared.removed += 1,
            Ok(Some(_)) | Err(_) => cleared.left += 1,
        }
    }
    cleared
}

#[cfg(test)]
mod tests;
