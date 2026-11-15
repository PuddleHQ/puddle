// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_types::{CollectingSink, Event};

use super::*;

fn url(text: &str) -> Url {
    Url::parse(text).unwrap()
}

fn problems() -> (Arc<CollectingSink>, Problems) {
    let sink = Arc::new(CollectingSink::default());
    (sink.clone(), Problems::new(sink))
}

#[test]
fn a_failed_open_names_the_host_and_the_reason_but_not_the_query() {
    let p = open_failed(&url("https://example.org/a?token=s3cret"), "no browser");
    assert_eq!(p.key, PROBLEM_KEY);
    assert!(p.title.contains("example.org"), "{}", p.title);
    assert!(p.detail.starts_with("no browser."), "{}", p.detail);
    assert!(!format!("{p:?}").contains("s3cret"));
}

#[test]
fn a_refused_link_names_the_scheme_only() {
    let p = refused(&url("file:///etc/passwd"));
    assert!(p.title.contains("file:"), "{}", p.title);
    assert!(!format!("{p:?}").contains("passwd"));
    let p = refused(&url("mailto:a@example.org"));
    assert!(!format!("{p:?}").contains("example.org"));
}

#[test]
fn an_address_without_a_host_is_named_by_its_scheme() {
    let p = open_failed(&url("http://[::1]:80/"), "x");
    assert!(p.title.contains("[::1]"), "{}", p.title);
    assert_eq!(where_to(&url("data:text/plain,hi")), "data:");
}

fn workspace(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[test]
fn a_failed_window_names_the_workspace_and_the_reason_not_a_link() {
    let p = window_failed(&workspace("demo"), "the webview refused the label");
    assert_eq!(p.key, "app:window:demo");
    assert_eq!(p.workspace, Some(workspace("demo")));
    assert!(p.title.contains("demo"), "{}", p.title);
    assert!(!p.title.contains("link"), "{}", p.title);
    assert!(
        p.detail.starts_with("the webview refused the label."),
        "{}",
        p.detail
    );
}

#[test]
fn a_failed_window_is_listed_once_per_workspace_and_a_new_window_withdraws_it() {
    let (_, problems) = problems();
    report_window_failed(&problems, &workspace("demo"), "first");
    report_window_failed(&problems, &workspace("demo"), "second");
    report_window_failed(&problems, &workspace("other"), "x");
    let listed = problems.list();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().any(|p| p.detail.starts_with("second.")));
    window_opened(&problems, &workspace("demo"));
    assert_eq!(problems.list().len(), 1);
    // The link problem is a different one and stays.
    report_refused(&problems, &url("javascript:alert(1)"));
    window_opened(&problems, &workspace("other"));
    assert_eq!(problems.list().len(), 1);
    assert_eq!(problems.list()[0].key, PROBLEM_KEY);
}

#[test]
fn a_refused_link_is_listed_and_a_repeat_is_announced_again() {
    let (sink, problems) = problems();
    report_refused(&problems, &url("javascript:alert(1)"));
    assert_eq!(problems.list(), [refused(&url("javascript:alert(1)"))]);
    let _ = sink.take();
    report_refused(&problems, &url("javascript:alert(1)"));
    assert_eq!(problems.list().len(), 1);
    assert_eq!(
        sink.take(),
        [Event::ProblemsChanged {}, Event::ProblemsChanged {}]
    );
}

#[test]
fn a_failed_open_is_listed_and_the_next_success_withdraws_it() {
    let (_, problems) = problems();
    let link = url("https://example.org/");
    open_and_report(&problems, &link, |u| {
        assert_eq!(u, "https://example.org/");
        Err("no browser")
    });
    let listed = problems.list();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].detail.starts_with("no browser."));
    open_and_report::<&str>(&problems, &link, |_| Ok(()));
    assert_eq!(problems.list(), []);
}

#[test]
fn a_success_with_nothing_standing_says_nothing() {
    let (sink, problems) = problems();
    open_and_report::<&str>(&problems, &url("https://example.org/"), |_| Ok(()));
    assert_eq!(sink.take(), []);
}
