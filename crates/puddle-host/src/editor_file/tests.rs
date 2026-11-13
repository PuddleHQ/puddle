// SPDX-License-Identifier: AGPL-3.0-or-later
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
