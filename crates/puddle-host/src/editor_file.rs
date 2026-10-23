// SPDX-License-Identifier: GPL-3.0-or-later
//! What the editor opens in a workspace: its start folder, or one window for every checkout.
//!
//! With no start folder set and two or more checkouts on the volume, the editor opens a multi-root
//! file, `.puddle/editor/<id>.code-workspace`, with one folder per checkout. Puddle owns only the
//! file's `folders` key: the boot hook merges it into whatever the user saved there (settings from
//! the editor, comments), and leaves a file it cannot parse as it is. The file sits inside
//! `.puddle`, which the delete check skips, so it can always be written again.

use puddle_api::StartFolder;
use puddle_types::{
    CheckoutDir, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec, WorkspaceId,
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
    let mut dirs: Vec<&CheckoutDir> = list_order
        .iter()
        .filter(|d| found.checkouts.contains(d))
        .collect();
    let mut rest: Vec<&CheckoutDir> = found
        .checkouts
        .iter()
        .filter(|d| !list_order.contains(d))
        .collect();
    rest.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    dirs.extend(rest);
    let path = |dir: &str| GuestPath::new(&format!("{}/{dir}", root.as_str())).ok();
    let mut folders: Vec<_> = dirs
        .into_iter()
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

/// The multi-root file of workspace `id` under `root`, with `folders` as the only key Puddle owns.
/// Readable by every user of the guest, so the editor and a desktop editor over SSH can open it.
pub(crate) fn workspace_file(
    root: &GuestPath,
    id: &WorkspaceId,
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
    let spec = MergeSpec::new(
        MergeFormat::Jsonc,
        vec![MergeEntry::json(&["folders"], &value)],
    )
    .ok()?;
    GuestFile::merged(path, spec).with_mode(0o644).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use puddle_types::Merged;

    fn root() -> GuestPath {
        GuestPath::new("/workspaces/shop").unwrap()
    }

    fn dirs(names: &[&str]) -> Vec<CheckoutDir> {
        names.iter().map(|n| CheckoutDir::new(n).unwrap()).collect()
    }

    fn found(names: &[&str], loose: bool) -> Found {
        Found {
            checkouts: dirs(names),
            loose,
        }
    }

    fn paths(f: &[(GuestPath, Option<&str>)]) -> Vec<String> {
        f.iter().map(|(p, _)| p.as_str().to_owned()).collect()
    }

    #[test]
    fn the_start_folder_or_the_root_is_where_terminals_land() {
        assert_eq!(start_path(&root(), None).as_str(), "/workspaces/shop");
        let sub = StartFolder::parse("web/src").unwrap();
        assert_eq!(
            start_path(&root(), Some(&sub)).as_str(),
            "/workspaces/shop/web/src"
        );
    }

    #[test]
    fn zero_or_one_checkout_or_a_start_folder_opens_a_folder() {
        let folders =
            |start: Option<&StartFolder>, f: &Found| multi_root_folders(&root(), start, f, &[]);
        assert_eq!(folders(None, &found(&[], true)), None);
        assert_eq!(folders(None, &found(&["api"], true)), None);
        let start = StartFolder::parse("api").unwrap();
        assert_eq!(folders(Some(&start), &found(&["api", "web"], false)), None);
        assert!(folders(None, &found(&["api", "web"], false)).is_some());
    }

    #[test]
    fn the_list_comes_first_then_the_others_by_name_and_the_root_only_with_loose_files() {
        let f = found(&["web", "api", "docs", "tools"], true);
        let order = dirs(&["web", "gone", "api"]);
        let folders = multi_root_folders(&root(), None, &f, &order).unwrap();
        assert_eq!(
            paths(&folders),
            [
                "/workspaces/shop/web",
                "/workspaces/shop/api",
                "/workspaces/shop/docs",
                "/workspaces/shop/tools",
                "/workspaces/shop"
            ]
        );
        assert_eq!(folders.last().unwrap().1, Some(ROOT_LABEL));
        let tidy = multi_root_folders(&root(), None, &found(&["b", "a"], false), &[]).unwrap();
        assert_eq!(paths(&tidy), ["/workspaces/shop/a", "/workspaces/shop/b"]);
    }

    #[test]
    fn the_file_owns_folders_and_keeps_every_other_key_and_comment() {
        let folders = multi_root_folders(&root(), None, &found(&["a", "b"], false), &[]).unwrap();
        let id = WorkspaceId::new("shop").unwrap();
        let file = workspace_file(&root(), &id, &folders).unwrap();
        assert_eq!(
            file.path().as_str(),
            "/workspaces/shop/.puddle/editor/shop.code-workspace"
        );
        assert_eq!(file.mode(), 0o644);
        let puddle_types::ApplyKind::Merge(spec) = file.apply() else {
            panic!("a merged file")
        };
        let user = b"{\n  // mine\n  \"settings\": {\"editor.tabSize\": 2},\n  \"folders\": [{\"path\": \"/old\"}],\n}";
        let Merged::Write(out) = spec.apply(Some(user), &[]).unwrap() else {
            panic!("written")
        };
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("// mine") && out.contains("\"editor.tabSize\": 2"),
            "{out}"
        );
        assert!(
            !out.contains("/old")
                && out.contains("/workspaces/shop/a")
                && out.contains("/workspaces/shop/b"),
            "{out}"
        );
        // A file that cannot be parsed is refused, and the hook leaves it as it is.
        assert!(spec.apply(Some(b"{ not json"), &[]).is_err());
    }

    fn checkout(dir: &str) -> puddle_workspace::FoundCheckout {
        puddle_workspace::FoundCheckout {
            dir: dir.to_owned(),
            remote_url: None,
            remote_note: None,
            branch: None,
            head: None,
            dirty: false,
            unreadable: None,
        }
    }

    #[test]
    fn the_checkout_listing_gives_the_folders_and_whether_anything_else_is_there() {
        let mut list = CheckoutList {
            checkouts: vec![checkout("api"), checkout("web")],
            ..CheckoutList::default()
        };
        let found = Found::from_listing(&list);
        assert_eq!(
            (found.checkouts, found.loose),
            (dirs(&["api", "web"]), false)
        );
        list.others = 3;
        assert!(Found::from_listing(&list).loose);
        // Checkouts past the listing's limit are not folders of the window: the root shows.
        list.others = 0;
        list.more = 5;
        assert!(Found::from_listing(&list).loose);
        // One the listing could not read is still a folder to open.
        let mut broken = checkout("broken");
        broken.unreadable = Some("git remote failed".to_owned());
        list.more = 0;
        list.checkouts.push(broken);
        assert_eq!(Found::from_listing(&list).checkouts.len(), 3);
    }

    #[test]
    fn a_name_that_is_not_a_folder_that_exists_is_left_to_the_root() {
        // `?` is what the listing leaves of a control character; `..` and a slash are no names.
        let list = CheckoutList {
            checkouts: vec![
                checkout("ok"),
                checkout("a?b"),
                checkout(".."),
                checkout("a/b"),
            ],
            ..CheckoutList::default()
        };
        let found = Found::from_listing(&list);
        assert_eq!((found.checkouts, found.loose), (dirs(&["ok"]), true));
    }

    #[test]
    fn the_file_has_one_place_per_workspace() {
        let id = WorkspaceId::new("shop").unwrap();
        assert_eq!(
            workspace_file_path(&root(), &id).unwrap().as_str(),
            "/workspaces/shop/.puddle/editor/shop.code-workspace"
        );
    }
}
