// SPDX-License-Identifier: GPL-3.0-or-later
//! What the user is told when a link in the window goes nowhere.
//!
//! A clicked link either opens in the default browser or it doesn't: the opener can fail, and the
//! window refuses anything that isn't a plain web address. Neither shows anything on its own, and
//! a desktop user never sees stderr, so both are raised as a background problem. The page already
//! lists those as notices (`GET /api/problems`), so this needs no window API and no UI of its own.
//!
//! One problem key serves both: the newest failure replaces the one before, and the next link
//! that opens takes it away.

use std::fmt::Display;

use puddle_types::{Problem, Problems};
use url::Url;

/// The key of the problem about a link that went nowhere.
const PROBLEM_KEY: &str = "app:link";

/// The problem for a web address the default browser could not be asked to open. Only the host is
/// named: an address can carry a token in its query.
#[must_use]
pub fn open_failed(url: &Url, reason: &str) -> Problem {
    Problem::new(
        PROBLEM_KEY,
        format!(
            "puddle could not open a link in your browser ({})",
            where_to(url)
        ),
        format!(
            "{reason}. Nothing was opened. Check that a default web browser is set, then click \
             the link again."
        ),
    )
}

/// The problem for a link the window refuses to follow. Only the scheme is named: the rest of an
/// address is the page's to show.
#[must_use]
pub fn refused(url: &Url) -> Problem {
    Problem::new(
        PROBLEM_KEY,
        format!("puddle did not follow a link ({}:)", url.scheme()),
        "puddle only opens web addresses (http or https) from this window, so nothing was opened.",
    )
}

/// The host of `url`, or its scheme when it has none.
fn where_to(url: &Url) -> String {
    url.host_str()
        .map_or_else(|| format!("{}:", url.scheme()), str::to_owned)
}

/// Raises the notice for a refused link.
pub fn report_refused(problems: &Problems, url: &Url) {
    raise_fresh(problems, refused(url));
}

/// Opens `url` with `open` and tells the user when that fails; a success withdraws an earlier
/// notice.
pub fn open_and_report<E: Display>(
    problems: &Problems,
    url: &Url,
    open: impl FnOnce(&str) -> Result<(), E>,
) {
    match open(url.as_str()) {
        Ok(()) => problems.clear(PROBLEM_KEY),
        Err(err) => raise_fresh(problems, open_failed(url, &err.to_string())),
    }
}

/// Withdraws the problem before raising it, so a page that read the list in between sees it go and
/// come back. A page keeps a notice the user dismissed dismissed while the problem reads the same.
fn raise_fresh(problems: &Problems, problem: Problem) {
    problems.clear(PROBLEM_KEY);
    problems.raise(problem);
}

#[cfg(test)]
mod tests {
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
}
