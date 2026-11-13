// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn found(dir: &str, remote: Option<&str>) -> FoundCheckout {
    FoundCheckout {
        dir: dir.to_owned(),
        remote_url: remote.map(str::to_owned),
        remote_note: None,
        branch: None,
        head: None,
        dirty: false,
        unreadable: None,
    }
}

fn list(checkouts: Vec<FoundCheckout>) -> CheckoutList {
    CheckoutList {
        checkouts,
        ..CheckoutList::default()
    }
}

fn volume() -> VolumeName {
    VolumeName::new("ws-lost").unwrap()
}

#[test]
fn the_problem_names_every_disk_without_saying_it_holds_data() {
    assert!(problem(&[]).is_none());
    let one = problem(&[WorkspaceId::new("a").unwrap()]).unwrap();
    assert_eq!(one.key, PROBLEM_KEY);
    assert!(one.title.contains("1 workspace disk that"), "{}", one.title);
    assert!(one.detail.contains("ws-a"), "{}", one.detail);
    assert!(!one.detail.contains("remove the volumes yourself"));
    let two = problem(&[
        WorkspaceId::new("a").unwrap(),
        WorkspaceId::new("b").unwrap(),
    ])
    .unwrap();
    assert!(two.title.contains("2 workspace disks"), "{}", two.title);
    assert!(two.detail.contains("ws-a, ws-b"), "{}", two.detail);
    assert!(
        two.detail.contains("cannot tell from outside"),
        "{}",
        two.detail
    );
}

#[test]
fn readable_remotes_become_entries_once_each_in_directory_order() {
    let entries = entries_for(
        &list(vec![
            found("api", Some("https://example.org/a/api.git")),
            found("copy", Some("https://example.org/a/api.git")),
            found("web", Some("https://example.org/a/web")),
            found("ssh", Some("git@example.org:a/x.git")),
            found("bare", None),
        ]),
        Some(RepoUrl::parse("https://example.org/a/other").unwrap()),
        &volume(),
    )
    .unwrap();
    let shown: Vec<_> = entries
        .iter()
        .map(|e| (e.url.as_str().to_owned(), e.directory.to_string()))
        .collect();
    // The address the user gave is not needed, so it is not used.
    assert_eq!(
        shown,
        [
            ("https://example.org/a/api.git".to_owned(), "api".to_owned()),
            ("https://example.org/a/web".to_owned(), "web".to_owned()),
        ]
    );
}

#[test]
fn a_disk_with_no_readable_address_needs_one_and_names_the_only_checkout() {
    let none = entries_for(&list(vec![found("bare", None)]), None, &volume());
    assert!(
        matches!(none, Err(WorkspaceError::Invalid(ref m)) if m.contains("enter the address") && m.contains("ws-lost")),
        "{none:?}"
    );
    let url = RepoUrl::parse("https://example.org/a/api").unwrap();
    let only = entries_for(
        &list(vec![found("bare", None)]),
        Some(url.clone()),
        &volume(),
    )
    .unwrap();
    assert_eq!(only[0].directory.to_string(), "bare");
    let two = entries_for(
        &list(vec![found("one", None), found("two", None)]),
        Some(url.clone()),
        &volume(),
    )
    .unwrap();
    assert_eq!(two[0].directory.to_string(), "api");
    let empty = entries_for(&list(vec![]), Some(url), &volume()).unwrap();
    assert_eq!(empty[0].directory.to_string(), "api");
}

#[test]
fn an_unreadable_checkout_gives_no_entry_even_with_a_remote_and_is_never_the_named_one() {
    let mut broken = found("broken", Some("https://example.org/a/broken.git"));
    broken.unreadable = Some("not a git repository".to_owned());
    let none = entries_for(&list(vec![broken.clone()]), None, &volume());
    assert!(
        matches!(none, Err(WorkspaceError::Invalid(ref m)) if m.contains("enter the address")),
        "{none:?}"
    );
    let url = RepoUrl::parse("https://example.org/a/api").unwrap();
    // The unreadable one is not "the only checkout without an address": the readable
    // neighbour is, so the given address goes in its directory.
    let beside = entries_for(
        &list(vec![broken, found("bare", None)]),
        Some(url),
        &volume(),
    )
    .unwrap();
    assert_eq!(beside.len(), 1);
    assert_eq!(beside[0].directory.to_string(), "bare");
}
