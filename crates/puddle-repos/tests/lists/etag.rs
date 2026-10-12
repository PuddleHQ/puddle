// SPDX-License-Identifier: GPL-3.0-or-later
//! Asking again for a page of a GitHub list with the `ETag` it came with: a `304` is not counted
//! against the host's rate limit, so it stands in for the page kept from the last read.

use puddle_repos::{ApiReply, Freshness, ListState, ProblemKind, SourceList};

use crate::common::{REPOS_PATH, Rig, T0, binding, gh, identities, ok_json};

const PAGE2: &str = "/user/repos?per_page=100&sort=full_name&direction=asc&page=2";
const NEXT: &str = r#"<https://api.github.com/user/repos?per_page=100&sort=full_name&direction=asc&page=2>; rel="next""#;
const CANARY: &str = "CANARY-etag-token-9e12";
const MINUTE: u64 = 60_000;

fn page1(etag: &str) -> ApiReply {
    ok_json("github_user_repos_page1.json")
        .with_header("link", NEXT)
        .with_header("etag", etag)
}

fn page2(etag: &str) -> ApiReply {
    ok_json("github_user_repos_page2.json").with_header("etag", etag)
}

fn unchanged() -> ApiReply {
    ApiReply::new(304, "")
}

fn names(list: &SourceList) -> Vec<&str> {
    list.repos.iter().map(|r| r.full_name.as_str()).collect()
}

fn signed_in() -> (Rig, Vec<puddle_store::Identity>) {
    let rig = Rig::new();
    let source = gh("github.com", "octocat");
    rig.tokens.set(&source, CANARY);
    let ids = identities(vec![("Me", vec![binding("github.com", source, &[], true)])]);
    (rig, ids)
}

fn sent(rig: &Rig) -> Vec<(String, Option<String>)> {
    rig.api
        .seen()
        .into_iter()
        .map(|s| (s.path, s.if_none_match))
        .collect()
}

#[tokio::test]
async fn a_refresh_asks_for_every_page_with_its_etag_and_a_304_keeps_the_page_and_renews_the_list()
{
    let (rig, ids) = signed_in();
    rig.api
        .reply("api.github.com", REPOS_PATH, page1(r#""one""#));
    rig.api.reply("api.github.com", REPOS_PATH, unchanged());
    rig.api.reply("api.github.com", PAGE2, page2(r#"W/"two""#));
    rig.api.reply("api.github.com", PAGE2, unchanged());

    let first = rig.lists(&ids, Freshness::Cached).await;
    rig.clock.advance(11 * MINUTE);
    let second = rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(
        sent(&rig),
        [
            (REPOS_PATH.to_owned(), None),
            (PAGE2.to_owned(), None),
            (REPOS_PATH.to_owned(), Some(r#""one""#.to_owned())),
            (PAGE2.to_owned(), Some(r#"W/"two""#.to_owned())),
        ]
    );
    assert_eq!(second[0].state, ListState::Ok);
    assert_eq!(second[0].refreshed_at, Some(T0 + 11 * MINUTE));
    assert_eq!(names(&second[0]).len(), 4);
    assert_eq!(names(&second[0]), names(&first[0]));
    assert_eq!(second[0].repos, first[0].repos);
    assert!(second[0].problem.is_none());
    assert!(!format!("{second:?}").contains(CANARY));
}

#[tokio::test]
async fn a_page_that_changed_replaces_the_kept_one_and_its_new_etag_is_the_one_sent_next() {
    let (rig, ids) = signed_in();
    rig.api
        .reply("api.github.com", REPOS_PATH, page1(r#""one""#));
    rig.api.reply("api.github.com", PAGE2, page2(r#""two""#));
    rig.lists(&ids, Freshness::Cached).await;

    // The second page changed (a repository less); the first is unchanged.
    rig.api.clear("api.github.com", REPOS_PATH);
    rig.api.reply("api.github.com", REPOS_PATH, unchanged());
    rig.api.clear("api.github.com", PAGE2);
    rig.api.reply(
        "api.github.com",
        PAGE2,
        ApiReply::new(200, r#"[{"full_name":"acme-corp/docs"}]"#).with_header("etag", r#""three""#),
    );
    rig.clock.advance(11 * MINUTE);
    let changed = rig.lists(&ids, Freshness::Cached).await;
    assert_eq!(changed[0].state, ListState::Ok);
    assert_eq!(names(&changed[0]).len(), 3);
    assert_eq!(names(&changed[0]).last(), Some(&"acme-corp/docs"));

    rig.api.clear("api.github.com", PAGE2);
    rig.api.reply("api.github.com", PAGE2, unchanged());
    rig.clock.advance(11 * MINUTE);
    let again = rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(rig.api.seen().last().unwrap().path, PAGE2);
    assert_eq!(
        rig.api.seen().last().unwrap().if_none_match.as_deref(),
        Some(r#""three""#)
    );
    assert_eq!(again[0].repos, changed[0].repos);
}

#[tokio::test]
async fn a_page_with_no_etag_is_asked_for_in_full_next_time() {
    let (rig, ids) = signed_in();
    rig.api.reply(
        "api.github.com",
        REPOS_PATH,
        ok_json("github_user_repos_page2.json"),
    );
    rig.lists(&ids, Freshness::Cached).await;
    rig.clock.advance(11 * MINUTE);
    rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(
        sent(&rig),
        [(REPOS_PATH.to_owned(), None), (REPOS_PATH.to_owned(), None)]
    );
}

#[tokio::test]
async fn a_page_the_list_no_longer_has_is_forgotten_with_its_etag() {
    let (rig, ids) = signed_in();
    rig.api
        .reply("api.github.com", REPOS_PATH, page1(r#""one""#));
    rig.api.reply("api.github.com", PAGE2, page2(r#""two""#));
    rig.lists(&ids, Freshness::Cached).await;

    // The list got shorter: one page and no link to another.
    rig.api.clear("api.github.com", REPOS_PATH);
    rig.api.reply(
        "api.github.com",
        REPOS_PATH,
        ok_json("github_user_repos_page2.json").with_header("etag", r#""short""#),
    );
    rig.clock.advance(11 * MINUTE);
    let shorter = rig.lists(&ids, Freshness::Cached).await;
    assert_eq!(shorter[0].repos.len(), 2);

    // And grew again: the page that came back is read in full, not against a stale validator.
    rig.api.clear("api.github.com", REPOS_PATH);
    rig.api
        .reply("api.github.com", REPOS_PATH, page1(r#""one""#));
    rig.clock.advance(11 * MINUTE);
    let longer = rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(longer[0].repos.len(), 4);
    let seen = sent(&rig);
    assert_eq!(seen.last().unwrap(), &(PAGE2.to_owned(), None));
}

#[tokio::test]
async fn a_304_for_a_page_that_was_asked_in_full_is_a_bad_answer_and_the_old_list_stays() {
    let (rig, ids) = signed_in();
    rig.api.reply("api.github.com", REPOS_PATH, unchanged());

    let lists = rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(lists[0].state, ListState::Failed);
    let problem = lists[0].problem.as_ref().unwrap();
    assert_eq!(problem.kind, ProblemKind::BadAnswer);
    assert!(problem.message.contains("unchanged"), "{}", problem.message);
    assert!(lists[0].repos.is_empty());
}

#[tokio::test]
async fn a_304_that_shows_the_hours_budget_spent_holds_the_next_read_back_like_a_200() {
    let (rig, ids) = signed_in();
    rig.api.reply(
        "api.github.com",
        REPOS_PATH,
        ok_json("github_user_repos_page2.json").with_header("etag", r#""one""#),
    );
    rig.lists(&ids, Freshness::Cached).await;
    let reset = (T0 + 30 * MINUTE) / 1000;
    rig.api.clear("api.github.com", REPOS_PATH);
    rig.api.reply(
        "api.github.com",
        REPOS_PATH,
        unchanged()
            .with_header("x-ratelimit-remaining", "0")
            .with_header("x-ratelimit-reset", &reset.to_string()),
    );
    rig.clock.advance(11 * MINUTE);

    let lists = rig.lists(&ids, Freshness::Cached).await;

    assert_eq!(lists[0].state, ListState::Ok);
    assert_eq!(lists[0].retry_at, Some(reset * 1000));
    assert_eq!(lists[0].repos.len(), 2);
}
