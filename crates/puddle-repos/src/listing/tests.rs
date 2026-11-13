// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::model::{ListState, Role, Visibility};

fn repo(host: &str, full_name: &str) -> Repository {
    let (owner, name) = full_name.split_once('/').unwrap();
    Repository {
        host: host.to_owned(),
        owner: owner.to_owned(),
        project: None,
        name: name.to_owned(),
        full_name: full_name.to_owned(),
        url: format!("https://{host}/{full_name}"),
        visibility: Visibility::Private,
        role: Role::Write,
        archived: false,
        fork: false,
    }
}

fn list(identity: i64, repos: Vec<Repository>) -> SourceList {
    SourceList {
        identity: IdentityId(identity),
        credential: 0,
        host: "github.com".to_owned(),
        organisation: None,
        state: ListState::Ok,
        refreshed_at: Some(1),
        retry_at: None,
        problem: None,
        notes: Vec::new(),
        repos: Arc::from(repos),
    }
}

fn query(text: &str, only: Option<i64>, offset: usize, limit: usize) -> Query {
    Query {
        text: text.to_owned(),
        only: only.map(IdentityId),
        offset,
        limit,
    }
}

fn names(page: &Page) -> Vec<&str> {
    page.repos
        .iter()
        .map(|f| f.repository.full_name.as_str())
        .collect()
}

fn lists() -> Vec<SourceList> {
    vec![
        list(
            1,
            vec![
                repo("github.com", "acme/web"),
                repo("github.com", "Acme/api"),
            ],
        ),
        list(
            2,
            vec![
                repo("github.com", "ACME/Web"),
                repo("github.com", "me/notes"),
            ],
        ),
    ]
}

#[test]
fn a_repository_two_identities_reach_appears_once_with_both_and_the_list_is_by_name() {
    let page = page(&lists(), &query("", None, 0, 50));
    assert_eq!(page.total, 3);
    assert_eq!(names(&page), ["Acme/api", "acme/web", "me/notes"]);
    // The first list's spelling stays, and every identity that lists it is named.
    assert_eq!(page.repos[1].identities, [IdentityId(1), IdentityId(2)]);
    assert_eq!(page.repos[2].identities, [IdentityId(2)]);
}

#[test]
fn the_same_identity_twice_is_named_once() {
    let page = page(
        &[
            list(1, vec![repo("github.com", "a/b")]),
            list(1, vec![repo("github.com", "A/B")]),
        ],
        &query("", None, 0, 50),
    );
    assert_eq!(page.repos.len(), 1);
    assert_eq!(page.repos[0].identities, [IdentityId(1)]);
}

#[test]
fn every_word_must_appear_in_the_full_name_whatever_the_case() {
    let all = lists();
    assert_eq!(
        names(&page(&all, &query("ACME", None, 0, 50))),
        ["Acme/api", "acme/web"]
    );
    assert_eq!(
        names(&page(&all, &query("  acme   WEB ", None, 0, 50))),
        ["acme/web"]
    );
    assert_eq!(page(&all, &query("nothing here", None, 0, 50)).total, 0);
}

#[test]
fn one_identity_sees_what_it_reaches_with_all_who_reach_it() {
    let all = lists();
    let mine = page(&all, &query("", Some(2), 0, 50));
    assert_eq!(names(&mine), ["acme/web", "me/notes"]);
    assert_eq!(mine.repos[0].identities, [IdentityId(1), IdentityId(2)]);
    assert_eq!(page(&all, &query("", Some(9), 0, 50)).total, 0);
}

#[test]
fn a_page_is_cut_after_the_total_is_counted() {
    let all = lists();
    let second = page(&all, &query("", None, 1, 1));
    assert_eq!(second.total, 3);
    assert_eq!(names(&second), ["acme/web"]);
    assert_eq!(page(&all, &query("", None, 5, 10)).repos, []);
}

#[test]
fn two_hosts_with_one_name_stay_apart_and_sort_by_host() {
    let page = page(
        &[list(
            1,
            vec![repo("b.example", "a/x"), repo("a.example", "a/x")],
        )],
        &query("", None, 0, 50),
    );
    let hosts: Vec<&str> = page
        .repos
        .iter()
        .map(|f| f.repository.host.as_str())
        .collect();
    assert_eq!(hosts, ["a.example", "b.example"]);
}
