// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's shell init as the host applies it.
//!
//! The text the user saved lives in the database and nowhere else is the truth. At every start
//! Puddle writes it to a file in a folder of its own for that workspace, mounts the folder
//! read-only in the guest, and has a login shell source the file from a line in a file Puddle owns
//! (see [`loader`]). The guest cannot change the text: the mount is read-only, and the lines that
//! load it are written again by the boot hook at every start.
//!
//! If the mount were ever writable the guest would be writing to the host's disk, so the host
//! never trusts the folder: [`Copies::publish`] compares what it finds with the database, puts
//! the database's text back, removes anything else from the folder and reports that it had to.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_compute::{ExecOutput, ExecRequest, FileMount};
use puddle_store::Store;
use puddle_types::{GuestFile, SessionUser, WorkspaceId, guest_path};

/// Where a workspace's folder with its shell init is mounted.
pub(crate) const GUEST_DIR: &str = "/puddle/shell-init";

/// The file in the folder that login shells source.
const FILE_NAME: &str = "init.sh";

/// The file Puddle owns in the guest that loads the shell init.
pub(crate) const LOADER_GUEST: &str = "/etc/profile.d/02-puddle-shell-init.sh";

/// How long the guest may take to run the user's script once (the check on save, and at start).
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// The file that makes a login shell source the shell init. A missing file does nothing, so a
/// workspace whose copy is not there yet starts all the same.
pub(crate) fn loader() -> GuestFile {
    let text = format!(
        "# Loads the shell init that Puddle keeps for this workspace. The copy it loads is read-only\n\
         # here: change it in puddle.\n\
         if [ -r {GUEST_DIR}/{FILE_NAME} ]; then\n    . {GUEST_DIR}/{FILE_NAME}\nfi\n"
    );
    GuestFile::new(guest_path!(LOADER_GUEST), text.into_bytes())
}

/// What [`Copies::publish`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Published {
    /// The folder held the database's text and nothing else (or nothing yet).
    Clean,
    /// It held something else and Puddle put the database's text back.
    Restored,
}

/// The folders that carry each workspace's shell init to its guest.
pub(crate) struct Copies {
    root: PathBuf,
    store: Arc<Store>,
    /// One writer at a time: a save and a start both publish, and one's cleanup of the folder
    /// would remove the other's half-written file. Holds the workspaces deleted since their
    /// last start, so a save that found a workspace just before its delete cannot make the
    /// folder again.
    writing: Mutex<BTreeSet<WorkspaceId>>,
}

impl Copies {
    /// Folders below `root` (which must be below the guest share), texts from `store`.
    pub(crate) fn new(root: PathBuf, store: Arc<Store>) -> Self {
        Self {
            root,
            store,
            writing: Mutex::new(BTreeSet::new()),
        }
    }

    fn dir(&self, id: &WorkspaceId) -> PathBuf {
        self.root.join(id.as_str())
    }

    /// The read-only mount that shows `id`'s folder at [`GUEST_DIR`].
    pub(crate) fn mount(&self, id: &WorkspaceId) -> FileMount {
        FileMount::read_only_dir(self.dir(id), guest_path!(GUEST_DIR))
    }

    /// Makes `id`'s folder hold exactly the text saved for `id` (an empty file when there is
    /// none) and nothing else. For a start: a workspace made again under a deleted id gets its
    /// folder from here.
    ///
    /// # Errors
    /// A message for the user when the database cannot be read or the folder cannot be written.
    pub(crate) fn publish(&self, id: &WorkspaceId) -> Result<Published, String> {
        self.write(id, true)
    }

    /// Like [`Copies::publish`], for a save: nothing is written for a workspace that was deleted
    /// and not started since.
    ///
    /// # Errors
    /// A message for the user when the database cannot be read or the folder cannot be written.
    pub(crate) fn refresh(&self, id: &WorkspaceId) -> Result<Published, String> {
        self.write(id, false)
    }

    fn write(&self, id: &WorkspaceId, revive: bool) -> Result<Published, String> {
        // Held from the read of the text, so a later save is never overwritten by an older one.
        let mut deleted = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if revive {
            deleted.remove(id);
        } else if deleted.contains(id) {
            return Ok(Published::Clean);
        }
        let wanted = self
            .store
            .shell_init(id)
            .map_err(|e| format!("cannot read the saved shell init: {e}"))?
            .map_or_else(String::new, |saved| saved.content.as_str().to_owned());
        publish_in(&self.dir(id), &wanted)
            .map_err(|e| format!("cannot write the shell init for the workspace: {e}"))
    }

    /// The text saved for `id`, if any.
    ///
    /// # Errors
    /// A message for the user when the database cannot be read.
    pub(crate) fn saved(&self, id: &WorkspaceId) -> Result<Option<String>, String> {
        self.store
            .shell_init(id)
            .map(|saved| saved.map(|s| s.content.as_str().to_owned()))
            .map_err(|e| format!("cannot read the saved shell init: {e}"))
    }

    /// Removes `id`'s folder (the workspace is gone). A folder that is not there is fine.
    pub(crate) fn forget(&self, id: &WorkspaceId) {
        let mut deleted = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        deleted.insert(id.clone());
        let dir = self.dir(id);
        if let Err(e) = fs::remove_dir_all(&dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %dir.display(), error = %e, "a deleted workspace's shell init folder was not removed");
        }
    }
}

/// Makes `dir` hold `FILE_NAME` with `wanted` and nothing else. Nothing in it is followed or read
/// through a link: a guest that got write access could plant one.
fn publish_in(dir: &Path, wanted: &str) -> std::io::Result<Published> {
    fs::create_dir_all(dir)?;
    let mut restored = false;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let is_file = entry.file_type()?.is_file();
        if is_file && entry.file_name() == FILE_NAME {
            // The text is compared below, through the same entry.
            continue;
        }
        restored = true;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    let target = dir.join(FILE_NAME);
    let held = match fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_file() => fs::read(&target).ok(),
        _ => None,
    };
    if held.as_deref() == Some(wanted.as_bytes()) {
        return Ok(if restored {
            Published::Restored
        } else {
            Published::Clean
        });
    }
    // A file that is not there yet is a first start, not a change by the guest.
    restored |= fs::symlink_metadata(&target).is_ok();
    // Written beside the target and renamed over it: a link at the target is replaced, never
    // followed, and a reader never sees half a text. `create_new` refuses a name that is taken,
    // links included.
    let partial = dir.join(".init.sh.new");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    file.write_all(wanted.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&partial, &target)?;
    Ok(if restored {
        Published::Restored
    } else {
        Published::Clean
    })
}

/// What running the user's script in a throwaway shell in the guest showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// It parsed and printed nothing.
    Quiet,
    /// It does not parse, or it printed something, or it ran too long; the text says what.
    Refused(String),
}

/// The guest side of the check. The script is on stdin; the exit status says what happened:
/// 0 quiet, 65 syntax error, 66 printed something, 70 could not set up. What it printed is on
/// stdout. Bash when the image has it (people write bash), else `sh`.
const CHECK_SCRIPT: &str = r#"f=$(mktemp) || exit 70
trap 'rm -f "$f"' EXIT
cat >"$f" || exit 70
if command -v bash >/dev/null 2>&1; then s=bash; else s=sh; fi
if ! out=$("$s" -n "$f" 2>&1); then printf '%s' "$out"; exit 65; fi
out=$("$s" -c '. "$1"' "$s" "$f" </dev/null 2>&1; echo x)
if [ "$out" != x ]; then printf '%s' "${out%x}"; exit 66; fi
"#;

/// The request that runs [`CHECK_SCRIPT`] on `content` as `user`, the user whose login shells
/// source it.
pub(crate) fn check_request(content: &str, user: SessionUser) -> ExecRequest {
    let mut request = ExecRequest::new("sh", ["-c", CHECK_SCRIPT])
        .as_user(user.name())
        .with_timeout(CHECK_TIMEOUT);
    request.stdin = content.as_bytes().to_vec();
    request
}

/// The most of a script's output the user is shown.
const SHOWN_BYTES: usize = 2 * 1024;

/// Reads what the check said.
pub(crate) fn verdict(output: &ExecOutput) -> Result<Verdict, String> {
    let said = output.stdout_text();
    let said = said.trim_end();
    let mut shown: String = said.chars().take(SHOWN_BYTES).collect();
    if shown.len() < said.len() {
        shown.push_str(" ...");
    }
    match output.status.code {
        0 => Ok(Verdict::Quiet),
        65 => Ok(Verdict::Refused(format!(
            "the shell init does not parse: {shown}"
        ))),
        66 => Ok(Verdict::Refused(format!(
            "the shell init prints output when a shell starts, which breaks tools that read a \
             shell's output (VS Code Remote-SSH, scp, rsync); remove what prints, or print only \
             when the shell is interactive. It printed: {shown}"
        ))),
        code => Err(format!(
            "the check could not run in the workspace (exit {code}): {}",
            output.stderr_text().trim_end()
        )),
    }
}

/// The text that says the check did not finish in time.
pub(crate) fn timed_out_message() -> String {
    format!(
        "the shell init took more than {} seconds to finish when a shell started it, and every \
         shell the workspace starts waits for it; make it quicker, or move slow work into the \
         background",
        CHECK_TIMEOUT.as_secs()
    )
}

/// The login-shell files a restart found replaced, from the hook's report: only the one that
/// loads the shell init counts.
pub(crate) fn loader_replaced(replaced: &[&str]) -> bool {
    replaced
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .contains(LOADER_GUEST)
}

#[cfg(test)]
mod tests;
