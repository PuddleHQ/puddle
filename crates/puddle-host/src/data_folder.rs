// SPDX-License-Identifier: AGPL-3.0-or-later
//! The data folder and the database in it are owner-only, also on an install made before they
//! were: start-up fixes what it can and refuses to run when it can't.

use std::io;
use std::path::{Path, PathBuf};

use puddle_fs::private::{self, Exposed};

use crate::{HostError, HostPaths};

/// Makes the data folder, `puddle.db` and the database's `-wal` and `-shm` files owner-only,
/// creating the folder and the database file owner-only when they are missing. An existing
/// folder or file that others can reach is tightened, and one line is logged naming what was
/// changed.
///
/// # Errors
///
/// [`HostError::State`] when something can't be made owner-only (for example a folder another
/// account owns): Puddle does not run with wider permissions.
pub(crate) fn make_owner_only(paths: &HostPaths) -> Result<(), HostError> {
    let mut changed = Vec::new();
    let data = paths.data();
    note(&mut changed, data, private::tighten_dir(data))?;
    // Creating the database file here, owner-only, means SQLite never makes it with the
    // default permissions; its `-wal` and `-shm` files copy the database's.
    let store = paths.store();
    match private::create_file(&store) {
        Ok(file) => drop(file),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            note(&mut changed, &store, private::tighten_file(&store))?;
        }
        Err(err) => return Err(refuse(&store, &err)),
    }
    for suffix in ["-wal", "-shm"] {
        let mut side = store.clone().into_os_string();
        side.push(suffix);
        let side = PathBuf::from(side);
        note(&mut changed, &side, private::tighten_file(&side))?;
    }
    if !changed.is_empty() {
        tracing::warn!(
            changed = %changed.join("; "),
            "made Puddle's data folder owner-only (access for other accounts removed)"
        );
    }
    Ok(())
}

fn note(
    changed: &mut Vec<String>,
    path: &Path,
    result: io::Result<Option<Exposed>>,
) -> Result<(), HostError> {
    match result {
        Ok(None) => Ok(()),
        Ok(Some(was)) => {
            changed.push(format!("{} was {}", path.display(), describe(&was)));
            Ok(())
        }
        Err(err) => Err(refuse(path, &err)),
    }
}

fn describe(was: &Exposed) -> String {
    match was {
        Exposed::Mode(mode) => format!("open to other accounts (mode {mode:o})"),
        Exposed::Acl(why) => format!("open to other accounts ({why})"),
    }
}

fn refuse(path: &Path, err: &io::Error) -> HostError {
    HostError::State {
        what: "the data folder",
        reason: format!(
            "{} must be accessible to your account only, and Puddle could not make it so: \
             {err}. Puddle will not run with wider permissions; fix the folder's permissions \
             (or its owner) and start Puddle again",
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests;
