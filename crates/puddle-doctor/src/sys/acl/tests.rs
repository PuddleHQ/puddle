// SPDX-License-Identifier: AGPL-3.0-or-later
//! The samples in `fixtures/` are `ls -lde` output captured on a hosted macOS runner (macOS 26.7,
//! user `runner`); the folder part of a path is `DIR` where it was a temporary folder.

use super::*;

fn parse(name: &str) -> Vec<AclEntry> {
    let path = format!(
        "{}/src/sys/acl/fixtures/{name}.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap();
    parse_ls_lde(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Whether msb's rule would refuse a folder with this listing, for the user `runner`.
fn refused(name: &str) -> bool {
    grants(&parse(name), "runner").iter().any(|g| g.mutating)
}

#[test]
fn a_listing_without_an_acl_has_no_entries() {
    for name in ["no_acl", "stock_users"] {
        assert_eq!(parse(name), [], "{name}");
    }
}

#[test]
fn an_entry_is_read_as_index_qualifier_and_text() {
    let entries = parse("mutating_group_everyone");
    assert_eq!(
        entries,
        [AclEntry {
            index: 0,
            qualifier: Qualifier::Other("group:everyone".to_owned()),
            allow: true,
            mutating: true,
            text: "group:everyone allow add_file,add_subdirectory,delete_child".to_owned(),
        }]
    );
}

#[test]
fn a_user_entry_keeps_the_name() {
    assert_eq!(
        parse("own_user_grant")[0].qualifier,
        Qualifier::User("runner".to_owned())
    );
}

#[test]
fn several_entries_keep_their_numbers_in_order() {
    let entries = parse("several_entries");
    let seen: Vec<(u32, bool, bool)> = entries
        .iter()
        .map(|e| (e.index, e.allow, e.mutating))
        .collect();
    assert_eq!(seen, [(0, false, true), (1, true, true), (2, true, false)]);
    // `ls` sorts deny entries first; the number to remove is the one it printed.
    let order: Vec<u32> = parse("reordered_entries").iter().map(|e| e.index).collect();
    assert_eq!(order, [0, 1, 2]);
}

#[test]
fn an_inherited_entry_is_parsed_like_any_other() {
    let entries = parse("inherited_entry");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].allow && entries[0].mutating);
}

#[test]
fn a_path_with_a_space_and_a_file_listing_parse() {
    assert_eq!(parse("path_with_space").len(), 1);
    assert!(parse("file_entry")[0].mutating);
}

#[test]
fn msbs_rule_over_the_samples() {
    // Refused: another account (a group, everyone, a named user) can change the folder.
    for name in [
        "mutating_group_everyone",
        "mutating_group_admin",
        "inheritable_grant",
        "every_permission",
    ] {
        assert!(refused(name), "{name}");
    }
    // Not refused: no ACL, this user, root, DENY entries, grants that only read.
    for name in [
        "no_acl",
        "own_user_grant",
        "root_grant",
        "deny_only",
        "deny_mutating",
        "read_only_everyone",
        "read_attr_only",
        "stock_home",
        "stock_library",
        "stock_users",
        "root_owned_folder",
    ] {
        assert!(!refused(name), "{name}");
    }
}

#[test]
fn a_user_named_like_this_one_in_the_reordered_listing_is_not_a_grant() {
    let found = grants(&parse("reordered_entries"), "runner");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].index, 1);
    // For a different user the same entry is another account's.
    assert_eq!(grants(&parse("reordered_entries"), "someone").len(), 2);
}

#[test]
fn a_named_user_that_is_not_this_one_counts() {
    let found = grants(&parse("named_user_and_group"), "runner");
    assert_eq!(found.len(), 2);
}

#[test]
fn a_grant_with_no_user_prefix_is_another_account() {
    let entries =
        parse_ls_lde("drwxr-xr-x+ 2 u g 64 Nov 15 15:27 d\n 0: 11111111-2222-3333-4444-555555555555 allow add_file\n")
            .unwrap();
    assert_eq!(grants(&entries, "runner").len(), 1);
}

#[test]
fn output_that_is_not_an_acl_listing_is_an_error_not_no_grant() {
    for bad in [
        "",
        " 0: group:everyone allow add_file\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n 0: group:everyone allow\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n 0: group:everyone permit add_file\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n x: group:everyone allow add_file\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n 0: group:everyone allow teleport\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n 0: group:everyone allow add_file extra\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n nonsense\n",
        "drwx------ 2 u g 64 Nov 15 15:27 d\n 0: group:everyone inherited\n",
    ] {
        assert!(parse_ls_lde(bad).is_err(), "{bad:?}");
    }
}

/// On the hosted macOS runner: real `chmod +a` folders, read through the real probe path.
#[cfg(target_os = "macos")]
mod live {
    use super::*;
    use std::process::Command;

    fn folder_with(acl: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        if let Some(acl) = acl {
            let ok = Command::new("/bin/chmod")
                .args(["+a", acl])
                .arg(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "chmod +a {acl}");
        }
        dir
    }

    fn live_grants(dir: &tempfile::TempDir) -> Vec<Grant> {
        let me = current_user().unwrap();
        read_grants(dir.path(), &me).unwrap()
    }

    #[test]
    fn the_real_ls_output_gives_the_grants_msb_refuses() {
        let refused = folder_with(Some(
            "everyone allow add_file,add_subdirectory,delete_child",
        ));
        assert!(live_grants(&refused).iter().any(|g| g.mutating));

        let me = current_user().unwrap();
        let own = folder_with(Some(&format!("user:{me} allow add_file")));
        assert_eq!(live_grants(&own), []);

        let deny = folder_with(Some("everyone deny delete"));
        assert_eq!(live_grants(&deny), []);

        let read = folder_with(Some("everyone allow read,search"));
        let got = live_grants(&read);
        assert_eq!(got.len(), 1);
        assert!(!got[0].mutating);

        assert_eq!(live_grants(&folder_with(None)), []);
    }

    #[test]
    fn the_whole_probe_names_a_mutating_ancestor_and_clears_it() {
        let top = folder_with(Some("everyone allow add_file,delete_child"));
        let data = top.path().join("a/data");
        std::fs::create_dir_all(&data).unwrap();
        let facts = crate::sys::data_path(Some(&data));
        let crate::facts::DataPathFacts::Checked { blocked, .. } = facts else {
            panic!("{facts:?}");
        };
        let top_resolved = std::fs::canonicalize(top.path()).unwrap();
        assert!(
            blocked.iter().any(|b| b.path == top_resolved
                && matches!(b.by, crate::facts::BlockedBy::MutatingAcl { .. })),
            "{blocked:?}"
        );
    }
}
