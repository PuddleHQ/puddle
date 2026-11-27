// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the editor opens in a workspace: its start folder, or one window for every checkout.
//!
//! With no start folder set and two or more checkouts on the volume, the editor opens a multi-root
//! file, `.puddle/editor/<id>.code-workspace`, with one folder per checkout. Puddle owns only the
//! file's `folders` key and its `settings.window.title` key (the display name, so the window is not
//! named after the id the file is named by): the boot hook merges them into whatever the user saved
//! there (other settings from the editor, comments), and leaves a file it cannot parse as it is. The file sits inside
//! `.puddle`, which the delete check skips, so it can always be written again.

use puddle_services::{workspaces::StartFolder, workspaces::WorkspaceRecord};
use puddle_types::{
    CheckoutDir, FileMode, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec, WorkspaceId,
};
use puddle_workspace::CheckoutList;

/// Where the multi-root file lives, below the volume root.
const EDITOR_DIR: &str = ".puddle/editor";

/// The name the volume root shows under in the multi-root window.
const ROOT_LABEL: &str = "workspace files";

/// What the volume root holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Found {
    /// The checkouts, in the order the guest listed them (by name).
    pub(crate) checkouts: Vec<CheckoutDir>,
    /// Whether anything else is there (loose files, other folders).
    pub(crate) loose: bool,
}

impl Found {
    /// What the checkout listing (the one the checkouts call shows) says the volume root holds.
    /// A checkout the listing could not read still counts: it is a folder to open. A name the
    /// listing had to clean (`?` stands for a control character) is not a path that exists, and
    /// a name that is no checkout name, or a checkout past the listing's limit, is left to the
    /// volume root's own folder, which then shows.
    pub(crate) fn from_listing(list: &CheckoutList) -> Self {
        let mut found = Self {
            loose: list.others > 0 || list.more > 0,
            ..Self::default()
        };
        for checkout in &list.checkouts {
            match CheckoutDir::new(&checkout.dir) {
                Ok(dir) if !checkout.dir.contains('?') => found.checkouts.push(dir),
                _ => found.loose = true,
            }
        }
        found
    }
}

/// The folder new terminals land in, below the volume root: the start folder, or with none set
/// the one repository of a workspace that lists exactly one. `None` is the volume root.
pub(crate) fn terminal_folder(record: &WorkspaceRecord) -> Option<StartFolder> {
    match (&record.start_folder, record.repositories.as_slice()) {
        (Some(folder), _) => Some(folder.clone()),
        (None, [only]) => StartFolder::parse(only.directory.as_str()).ok(),
        (None, _) => None,
    }
}

/// Where a new terminal lands: the start folder below `root`, or `root` itself.
pub(crate) fn start_path(root: &GuestPath, start: Option<&StartFolder>) -> GuestPath {
    match start {
        // A start folder is a checked relative path, so the join is a valid guest path.
        Some(folder) => {
            GuestPath::new(&format!("{}/{folder}", root.as_str())).unwrap_or_else(|_| root.clone())
        }
        None => root.clone(),
    }
}

/// The folders of the multi-root window: the list's entries first (in list order), then the other
/// checkouts by name, and the volume root last when it holds anything besides checkouts. `None`
/// when the editor opens the start folder or the root instead: a start folder is set, or there
/// are fewer than two checkouts.
pub(crate) fn multi_root_folders(
    root: &GuestPath,
    start: Option<&StartFolder>,
    found: &Found,
    list_order: &[CheckoutDir],
) -> Option<Vec<(GuestPath, Option<&'static str>)>> {
    if start.is_some() || found.checkouts.len() < 2 {
        return None;
    }
    let mut rest: Vec<&CheckoutDir> = found
        .checkouts
        .iter()
        .filter(|d| !list_order.contains(d))
        .collect();
    rest.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let path = |dir: &str| GuestPath::new(&format!("{}/{dir}", root.as_str())).ok();
    let mut folders: Vec<_> = list_order
        .iter()
        .filter(|d| found.checkouts.contains(d))
        .chain(rest)
        .filter_map(|d| path(d.as_str()).map(|p| (p, None)))
        .collect();
    if found.loose {
        folders.push((root.clone(), Some(ROOT_LABEL)));
    }
    Some(folders)
}

/// Where the multi-root file of workspace `id` is, below the volume root `root`.
pub(crate) fn workspace_file_path(root: &GuestPath, id: &WorkspaceId) -> Option<GuestPath> {
    GuestPath::new(&format!(
        "{}/{EDITOR_DIR}/{id}.code-workspace",
        root.as_str()
    ))
    .ok()
}

/// The window title the multi-root file sets: VS Code's default title with the file's name (the
/// workspace id) replaced by the display name `label`. `None` when `label` holds `${`, which the
/// editor would read as a variable; the window then keeps the default title.
fn window_title(label: &str) -> Option<String> {
    (!label.contains("${")).then(|| {
        format!("${{dirty}}${{activeEditorShort}}${{separator}}{label}${{separator}}${{appName}}")
    })
}

/// The multi-root file of workspace `id` under `root`, with `folders` and the window title as the
/// only keys Puddle owns. Readable by every user of the guest, so the editor and a desktop editor over SSH can open it.
pub(crate) fn workspace_file(
    root: &GuestPath,
    id: &WorkspaceId,
    label: &str,
    folders: &[(GuestPath, Option<&'static str>)],
) -> Option<GuestFile> {
    let path = workspace_file_path(root, id)?;
    let value = serde_json::Value::Array(
        folders
            .iter()
            .map(|(folder, label)| match label {
                Some(name) => serde_json::json!({"path": folder.as_str(), "name": name}),
                None => serde_json::json!({"path": folder.as_str()}),
            })
            .collect(),
    );
    let mut entries = vec![MergeEntry::json(&["folders"], &value)];
    if let Some(title) = window_title(label) {
        entries.push(MergeEntry::json(
            &["settings", "window.title"],
            &serde_json::Value::String(title),
        ));
    }
    let spec = MergeSpec::new(MergeFormat::Jsonc, entries).ok()?;
    Some(GuestFile::merged(path, spec).with_mode(FileMode::new(0o644)))
}

#[cfg(test)]
mod tests;
