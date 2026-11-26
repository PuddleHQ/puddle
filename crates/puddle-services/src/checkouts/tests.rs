// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn entry(url: &str, dir: &str) -> Repository {
    Repository {
        entry: crate::new_entry_id(|_| false).unwrap(),
        url: RepoUrl::parse(url).unwrap(),
        directory: CheckoutDir::new(dir).unwrap(),
    }
}

fn found(dir: &str, url: Option<&str>, note: Option<RemoteNote>) -> Found {
    Found {
        directory: dir.to_owned(),
        remote_url: url.map(str::to_owned),
        remote_note: note,
        branch: Some("main".into()),
        head: Some("abc".into()),
        dirty: false,
        unreadable: None,
    }
}

fn view(listing: &FoundList, entries: &[Repository]) -> Checkouts {
    checkouts_view(true, Some(5), None, Some(listing), entries)
}

#[test]
fn a_checkout_that_could_not_be_read_or_has_a_bad_address_is_not_offered() {
    let mut unreadable = found("odd", None, None);
    unreadable.unreadable = Some("permission denied".to_owned());
    let listing = FoundList {
        checkouts: vec![
            unreadable,
            found("plain", Some("http://github.com/o/plain"), None),
        ],
        more: 0,
    };
    let v = view(&listing, &[]);
    let why = |d: &str| {
        let c = v.checkouts.iter().find(|c| c.found.directory == d).unwrap();
        assert!(!c.manage.offered);
        c.manage.reason.clone().unwrap()
    };
    assert_eq!(
        why("odd"),
        "Puddle could not read this checkout: permission denied"
    );
    assert!(why("plain").contains("https://"), "{}", why("plain"));
}

#[test]
fn addresses_of_a_host_the_table_cannot_name_are_compared_as_written() {
    let a = RepoUrl::parse("https://example.com:8443/a/b").unwrap();
    let same = RepoUrl::parse("https://example.com:8443/a/b").unwrap();
    let other = RepoUrl::parse("https://example.com:8443/a/c").unwrap();
    assert!(same_repository(&a, &same));
    assert!(!same_repository(&a, &other));
}

#[test]
fn each_checkout_has_its_entry_and_a_reason_when_it_cannot_be_managed() {
    let entries = [entry("https://github.com/o/api", "api")];
    let listing = FoundList {
        checkouts: vec![
            found("api", Some("https://github.com/o/api"), None),
            found("web", Some("https://github.com/o/web"), None),
            found("ssh", None, Some(RemoteNote::Ssh)),
            found("old", None, Some(RemoteNote::NotHttps)),
            found("bare", None, Some(RemoteNote::NoRemote)),
            found("again", Some("https://github.com/O/API"), None),
        ],
        more: 0,
    };
    let v = view(&listing, &entries);
    let by = |d: &str| v.checkouts.iter().find(|c| c.found.directory == d).unwrap();
    assert!(by("api").entry.is_some());
    assert!(!by("api").manage.offered);
    assert!(by("web").manage.offered && by("web").entry.is_none());
    assert_eq!(
        by("ssh").manage.reason.as_deref(),
        Some("its remote is an SSH address; Manage this needs an https:// one")
    );
    assert!(
        by("old")
            .manage
            .reason
            .as_deref()
            .unwrap()
            .contains("https://")
    );
    assert!(
        by("bare")
            .manage
            .reason
            .as_deref()
            .unwrap()
            .contains("no remote")
    );
    let again = by("again").manage.reason.clone().unwrap();
    assert!(again.contains("already on the list, in api"), "{again}");
    assert_eq!(v.missing, Vec::new());
}

#[test]
fn an_entry_without_a_checkout_is_missing_unless_the_listing_was_cut_short() {
    let entries = [entry("https://h/o/a", "a"), entry("https://h/o/b", "b")];
    let mut listing = FoundList {
        checkouts: vec![found("a", Some("https://h/o/a"), None)],
        more: 0,
    };
    let names =
        |v: &Checkouts| -> Vec<String> { v.missing.iter().map(ToString::to_string).collect() };
    assert_eq!(names(&view(&listing, &entries)), ["b"]);
    // An unreadable checkout is not proof the entry's folder is a checkout.
    listing.checkouts[0].unreadable = Some("boom".into());
    assert_eq!(names(&view(&listing, &entries)), ["a", "b"]);
    // Past the limit an entry may be among the checkouts left out.
    listing.checkouts[0].unreadable = None;
    listing.more = 3;
    let v = view(&listing, &entries);
    assert_eq!(v.missing, Vec::new());
    assert_eq!(v.more, 3);
}

#[test]
fn a_full_list_offers_nothing() {
    let entries: Vec<Repository> = (0..MAX_REPOSITORIES)
        .map(|i| entry(&format!("https://h/o/r{i}"), &format!("r{i}")))
        .collect();
    let listing = FoundList {
        checkouts: vec![found("extra", Some("https://h/o/extra"), None)],
        more: 0,
    };
    let v = view(&listing, &entries);
    assert!(
        v.checkouts[0]
            .manage
            .reason
            .as_deref()
            .unwrap()
            .contains("full")
    );
}

#[test]
fn no_listing_says_why() {
    let v = checkouts_view(false, None, Some("start it".into()), None, &[]);
    assert!(!v.running && v.checked_at.is_none());
    assert_eq!(v.problem.as_deref(), Some("start it"));
    assert_eq!(v.checkouts, Vec::new());
}

#[test]
fn an_addition_needs_room_a_free_directory_and_a_new_repository() {
    let entries = vec![entry("https://github.com/o/api", "api")];
    assert!(check_addition(&entries, &entry("https://github.com/o/web", "web")).is_ok());
    let err = check_addition(&entries, &entry("https://github.com/o/web", "api")).unwrap_err();
    assert!(err.to_string().contains("already on the list, as"), "{err}");
    let err =
        check_addition(&entries, &entry("https://github.com/O/API.git", "other")).unwrap_err();
    assert!(err.to_string().contains("in api"), "{err}");
    let mut same_id = entry("https://github.com/o/web", "web");
    same_id.entry = entries[0].entry.clone();
    let err = check_addition(&entries, &same_id).unwrap_err();
    assert!(err.to_string().contains("try again"), "{err}");
    let full: Vec<Repository> = (0..MAX_REPOSITORIES)
        .map(|i| entry(&format!("https://h/o/r{i}"), &format!("r{i}")))
        .collect();
    let err = check_addition(&full, &entry("https://h/o/new", "new")).unwrap_err();
    assert!(err.to_string().contains("full"), "{err}");
}
