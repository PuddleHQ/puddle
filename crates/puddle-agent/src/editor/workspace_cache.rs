// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor's cached copy of the multi-root file, put in place before a window reads it.
//!
//! **This mirrors private internals of VS Code's web workbench (as code-server 4.141 bundles it)
//! to work around a race in it. Remove the module and its callers once the workbench keeps the
//! folders on its own.**
//!
//! A web window that opens a multi-root file on the server builds its workspace from the
//! workbench's cached copy of that file, and reads the file itself in parallel. When the cache has
//! no entry, the workspace starts with no folders; if the file's read then answers before the
//! workbench has stored the workspace, the workbench drops it, and the window shows "No Folder
//! Opened" until it is reloaded. The workbench writes the entry itself only seconds after a window
//! opens, so the first window of a workspace on a busy machine is exposed. With the entry in place
//! the window starts from the cached folders, and the file's read only confirms them.
//!
//! The internals mirrored here:
//! - the entry is `<user data>/User/caches/CachedConfigurations/workspaces/<id>/workspace.json`,
//!   holding `{"content":"<the file's text>"}`;
//! - `<id>` is the workbench's `hash()` of the file's URI, `vscode-remote://<authority><path>`
//!   (the authority lower-cased, as `URI.toString()` writes it), printed as `toString(16)` of a
//!   signed 32-bit number, so it can start with `-`.
//!
//! The tests pin the hash against an id the workbench made itself, and the browser-editor VM test
//! checks that a window's own storage id has the entry this module wrote: a workbench that keys
//! its cache another way fails that test instead of bringing the race back unnoticed.

use std::io;
use std::path::{Path, PathBuf};

use puddle_agent_proto::editor::EditorServer;

/// Where the workbench keeps its cached copies of multi-root files, below the user data folder.
const WORKSPACES_CACHE: &str = "User/caches/CachedConfigurations/workspaces";

/// The file of one cached copy, in the folder named by the workspace's id.
const ENTRY_FILE: &str = "workspace.json";

/// Puts the cached copy of `server`'s multi-root file into the user data folder `user_data`, when
/// the server opens one at a known window address and the copy there differs. Never fails: a copy
/// that cannot be written leaves the window as exposed as it was without this module.
pub(super) fn fill(server: &EditorServer, user_data: &Path) {
    if let Err(err) = try_fill(server, user_data) {
        tracing::debug!(error = %err, "the editor's cached copy of the multi-root file was not written");
    }
}

fn try_fill(server: &EditorServer, user_data: &Path) -> io::Result<()> {
    let (Some(file), Some(authority)) = (&server.workspace_file, &server.window_authority) else {
        return Ok(());
    };
    let Some(entry) = entry_path(user_data, authority, file) else {
        return Ok(());
    };
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let content = serde_json::json!({ "content": text }).to_string();
    if std::fs::read_to_string(&entry).is_ok_and(|old| old == content) {
        return Ok(());
    }
    write_owned(user_data, &entry, &content)
}

/// The cached copy's path for the multi-root file `file` opened at `authority`; `None` when the
/// URI would need escaping (the workbench hashes the escaped form, which this does not rebuild).
fn entry_path(user_data: &Path, authority: &str, file: &str) -> Option<PathBuf> {
    let uri = workspace_uri(authority, file)?;
    Some(
        user_data
            .join(WORKSPACES_CACHE)
            .join(workspace_id(&uri))
            .join(ENTRY_FILE),
    )
}

/// `vscode-remote://<authority><path>` as the workbench writes it, or `None` when it would
/// escape a character of either part.
fn workspace_uri(authority: &str, path: &str) -> Option<String> {
    let plain = |part: &str, extra: &[u8]| {
        part.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~/".contains(&b) || extra.contains(&b))
    };
    let authority = authority.to_ascii_lowercase();
    (path.starts_with('/') && plain(path, b"") && !authority.is_empty() && plain(&authority, b":"))
        .then(|| format!("vscode-remote://{authority}{path}"))
}

/// The workbench's id of the workspace whose file is at `uri`: its string `hash()` (seed 149417,
/// `h = (h << 5) - h + unit` over the UTF-16 units, 32-bit) in base 16 with a sign.
fn workspace_id(uri: &str) -> String {
    let step = |value: i32, hash: i32| hash.wrapping_shl(5).wrapping_sub(hash).wrapping_add(value);
    let hash = uri
        .encode_utf16()
        .fold(step(149_417, 0), |hash, unit| step(i32::from(unit), hash));
    if hash < 0 {
        format!("-{:x}", hash.unsigned_abs())
    } else {
        format!("{hash:x}")
    }
}

/// Writes `content` to `entry`, making the folders below `user_data` it needs, all owned by the
/// owner of `user_data` (the editor's user, which must be able to update the copy later).
fn write_owned(user_data: &Path, entry: &Path, content: &str) -> io::Result<()> {
    let owner = Owner::of(user_data)?;
    let mut made = Vec::new();
    let mut missing = entry.parent();
    while let Some(dir) = missing.filter(|dir| *dir != user_data && !dir.exists()) {
        made.push(dir.to_path_buf());
        missing = dir.parent();
    }
    for dir in made.iter().rev() {
        std::fs::create_dir(dir)?;
        owner.give(dir)?;
    }
    puddle_fs::private::write_atomic(entry, content.as_bytes())?;
    owner.give(entry)
}

/// Who owns the user data folder.
struct Owner {
    #[cfg(unix)]
    ids: (u32, u32),
}

impl Owner {
    #[cfg(unix)]
    fn of(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt as _;
        let meta = std::fs::metadata(path)?;
        Ok(Self {
            ids: (meta.uid(), meta.gid()),
        })
    }

    #[cfg(not(unix))]
    fn of(path: &Path) -> io::Result<Self> {
        std::fs::metadata(path).map(|_| Self {})
    }

    #[cfg(unix)]
    fn give(&self, path: &Path) -> io::Result<()> {
        std::os::unix::fs::chown(path, Some(self.ids.0), Some(self.ids.1))
    }

    #[cfg(not(unix))]
    #[expect(
        clippy::unused_self,
        reason = "keeps the signature of the Unix version"
    )]
    #[expect(
        clippy::unnecessary_wraps,
        reason = "keeps the signature of the Unix version"
    )]
    fn give(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
