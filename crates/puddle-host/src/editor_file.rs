// SPDX-License-Identifier: GPL-3.0-or-later
//! What the editor opens in a workspace: its start folder, or one window for every checkout.
//!
//! With no start folder set and two or more checkouts on the volume, the editor opens a multi-root
//! file, `.puddle/editor/<id>.code-workspace`, with one folder per checkout. Puddle owns only the
//! file's `folders` key: the boot hook merges it into whatever the user saved there (settings from
//! the editor, comments), and leaves a file it cannot parse as it is. The file sits inside
//! `.puddle`, which the delete check skips, so it can always be written again.

use puddle_api::StartFolder;
use puddle_compute::ExecRequest;
use puddle_types::{
    CheckoutDir, GuestFile, GuestPath, MergeEntry, MergeFormat, MergeSpec, WorkspaceId,
};
use std::time::Duration;

/// Where the multi-root file lives, below the volume root.
const EDITOR_DIR: &str = ".puddle/editor";

/// The name the volume root shows under in the multi-root window.
const ROOT_LABEL: &str = "workspace files";

/// How long the guest may take to list the volume root.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Lists the entries directly under the volume root (`$1`): `R <name>` for each checkout, with the
/// delete check's test (a directory, not a link, holding `.git`), and `O <count>` for everything
/// else. Puddle's own folder and `lost+found` are not counted, and a name with a control
/// character counts as other, so every printed name is one line.
const LISTING_SH: &str = r#"cd -- "$1" || exit 3
others=0
for d in * .[!.]* ..?*; do
    [ -e "$d" ] || [ -L "$d" ] || continue
    case $d in .puddle | lost+found) continue ;; esac
    case $d in *[![:print:]]*) others=$((others + 1)); continue ;; esac
    if [ -d "$d" ] && [ ! -L "$d" ] && [ -e "$d/.git" ]; then
        printf 'R\t%s\n' "$d"
    else
        others=$((others + 1))
    fi
done
printf 'O\t%s\n' "$others"
"#;

/// The most checkouts one window shows.
const MAX_FOLDERS: usize = 100;

/// What the volume root holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Found {
    /// The checkouts, in the order the guest listed them (by name).
    pub(crate) checkouts: Vec<CheckoutDir>,
    /// Whether anything else is there (loose files, other folders).
    pub(crate) loose: bool,
}

/// The command that lists the volume root `root`.
pub(crate) fn listing_request(root: &GuestPath) -> ExecRequest {
    ExecRequest::new(
        "sh",
        ["-c", LISTING_SH, "puddle-editor-listing", root.as_str()].map(str::to_owned),
    )
    .as_user("root")
    .with_timeout(LIST_TIMEOUT)
}

/// Reads what [`listing_request`] printed. Guest output is input: a line that is not a record, a
/// listing that does not end with its `O` line, and a name that is no checkout name are refused or
/// counted as other.
///
/// # Errors
///
/// A message when the listing is cut short or holds something else than records.
pub(crate) fn parse_listing(stdout: &str) -> Result<Found, String> {
    let mut found = Found::default();
    let mut ended = false;
    let mut others = false;
    for line in stdout.lines() {
        if ended {
            return Err("the listing goes on after its last line".to_owned());
        }
        match line.split_once('\t') {
            Some(("R", name)) => match CheckoutDir::new(name) {
                Ok(dir) if found.checkouts.len() < MAX_FOLDERS => found.checkouts.push(dir),
                _ => others = true,
            },
            Some(("O", count)) => {
                others |= count
                    .parse::<u64>()
                    .map_err(|_| "a bad count of other entries")?
                    > 0;
                ended = true;
            }
            _ => return Err("the listing holds a line that is no record".to_owned()),
        }
    }
    if !ended {
        return Err("the listing ends early".to_owned());
    }
    found.loose = others;
    Ok(found)
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

/// The multi-root file of workspace `id` under `root`, with `folders` as the only key Puddle owns.
/// Readable by every user of the guest, so the editor and a desktop editor over SSH can open it.
pub(crate) fn workspace_file(
    root: &GuestPath,
    id: &WorkspaceId,
    folders: &[(GuestPath, Option<&'static str>)],
) -> Option<GuestFile> {
    let path = GuestPath::new(&format!(
        "{}/{EDITOR_DIR}/{id}.code-workspace",
        root.as_str()
    ))
    .ok()?;
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
    use std::fmt::Write as _;

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

    #[test]
    fn a_listing_is_read_as_guest_input() {
        let ok = parse_listing("R\tapi\nR\tweb\nO\t3\n").unwrap();
        assert_eq!((ok.checkouts, ok.loose), (dirs(&["api", "web"]), true));
        assert!(!parse_listing("R\tapi\nO\t0\n").unwrap().loose);
        // A name that is no checkout name counts as other.
        let odd = parse_listing("R\t..\nR\ta b/c\nO\t0\n").unwrap();
        assert_eq!((odd.checkouts.len(), odd.loose), (0, true));
        for bad in [
            "R\tapi\n",
            "",
            "X\tapi\nO\t0\n",
            "O\t0\nR\tapi\n",
            "O\tmany\n",
            "garbage\n",
        ] {
            assert!(parse_listing(bad).is_err(), "{bad:?}");
        }
        let many = (0..150).fold(String::new(), |mut out, i| {
            let _ = writeln!(out, "R\td{i}");
            out
        }) + "O\t0\n";
        let capped = parse_listing(&many).unwrap();
        assert_eq!((capped.checkouts.len(), capped.loose), (MAX_FOLDERS, true));
    }

    #[test]
    fn the_listing_script_finds_checkouts_not_links_and_counts_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let at = |p: &str| tmp.path().join(p);
        std::fs::create_dir_all(at("api/.git")).unwrap();
        std::fs::create_dir_all(at("web/.git")).unwrap();
        std::fs::create_dir_all(at("plain")).unwrap();
        std::fs::create_dir_all(at(".puddle/editor")).unwrap();
        std::fs::create_dir_all(at("lost+found")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(at("api"), at("link")).unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", LISTING_SH, "x"])
            .arg(tmp.path())
            .output()
            .unwrap();
        let found = parse_listing(&String::from_utf8(out.stdout).unwrap()).unwrap();
        let mut names: Vec<_> = found
            .checkouts
            .iter()
            .map(|d| d.as_str().to_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["api", "web"]);
        assert!(found.loose, "plain and the link are other entries");
        std::fs::remove_dir(at("plain")).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(at("link")).unwrap();
        let out = std::process::Command::new("sh")
            .args(["-c", LISTING_SH, "x"])
            .arg(tmp.path())
            .output()
            .unwrap();
        assert!(
            !parse_listing(&String::from_utf8(out.stdout).unwrap())
                .unwrap()
                .loose
        );
    }
}
