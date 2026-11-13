// SPDX-License-Identifier: AGPL-3.0-or-later
//! Housekeeping of msb's per-sandbox log files (`sandboxes/<name>/logs/`).

use std::path::Path;

/// Copies the files of `from` into `to`, best effort: a sandbox that never got a log directory
/// has nothing to keep, and a failed copy must not fail the removal it belongs to.
pub(crate) fn keep(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    if std::fs::create_dir_all(to).is_err() {
        return;
    }
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::copy(&path, to.join(entry.file_name()));
        }
    }
}

#[cfg(test)]
mod tests;
