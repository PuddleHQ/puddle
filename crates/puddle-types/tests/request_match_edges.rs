// SPDX-License-Identifier: GPL-3.0-or-later
//! Edges of the request-path matching that the unit tests next to the code do not pin down: the
//! exact readings a request target has, the limits of a path pattern and of a method list, and
//! that percent-decoding stops. Each assertion is the result a caller sees at the public API.
#![expect(
    clippy::expect_used,
    reason = "a helper outside #[test] fails the test by panicking"
)]

use std::sync::mpsc;
use std::time::Duration;

use puddle_types::{
    CanonicalPath, MAX_METHODS, MAX_PATH_PATTERN, MAX_PATH_SEGMENTS, Methods, PathPattern,
    PathPatternError,
};

/// Runs `work` on its own thread and fails the test when it has not finished in a second, so a
/// decoder that stops advancing is a failed test and not a hang.
fn bounded<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(Duration::from_secs(1))
        .expect("the work did not finish in a second")
}

/// The readings of `target` as sorted segment lists.
fn readings(target: &str) -> Vec<Vec<String>> {
    let target = target.to_owned();
    let mut all: Vec<Vec<String>> = bounded(move || {
        CanonicalPath::readings(&target)
            .iter()
            .map(|path| path.segments().to_vec())
            .collect()
    });
    all.sort();
    all
}

fn expect(target: &str, want: &[&[&str]]) {
    let mut want: Vec<Vec<String>> = want
        .iter()
        .map(|segments| segments.iter().map(|s| (*s).to_owned()).collect())
        .collect();
    want.sort();
    assert_eq!(readings(target), want, "{target:?}");
}

#[test]
fn a_dot_segment_is_resolved_in_every_reading_that_splits_first() {
    // Only `%` makes a reading split before decoding, so each target carries one.
    expect("/a/b/../%63", &[&["a", "c"]]);
    expect("/a/b/./../%63", &[&["a", "c"]]);
    expect("/a/b/./%63", &[&["a", "b", "c"]]);
    expect("/a/./b/%63", &[&["a", "b", "c"]]);
    // An escaped `..` is resolved by one family of servers and is a name for another.
    expect("/a/b/%2e%2e/%63", &[&["a", "c"], &["a", "b", "c"]]);
    expect("/a/b/%2e/../%63", &[&["a", "c"], &["a", "b", "c"]]);
    expect("/a/b/..%2f%63", &[&["a", "c"], &["a", "b", "../c"]]);
    // Resolving `..` never goes above the root.
    expect("/%2e%2e/../%61", &[&["a"]]);
    // A `.` or `..` with a control character or a space around it is one on Windows only.
    expect("/a/b/.. /%63", &[&["a", "c"], &["a", "b", "c"]]);
    expect("/a/b/.\u{1}/../%63", &[&["a", "c"], &["a", "b", "c"]]);
}

#[test]
fn a_dot_segment_is_resolved_in_the_whole_path_reading() {
    expect("/a/b/..", &[&["a"]]);
    expect("/a/./b", &[&["a", "b"]]);
    expect("/a/b/%2e%2e", &[&["a"], &["a", "b"]]);
}

/// Every spelling an attacker may give `/admin` is refused by the rule `/admin`, and the
/// neighbours of it are not.
#[test]
fn a_rule_on_one_path_refuses_every_spelling_of_it_and_only_those() {
    let rule = PathPattern::parse("/admin").unwrap();
    let refused = |target: &str| {
        readings_of(target)
            .iter()
            .any(|reading| rule.matches(reading))
    };
    for spelling in [
        "/admin",
        "/ADMIN",
        "/AdMiN/",
        "//admin",
        "///admin//",
        "/./admin",
        "/admin/.",
        "/admin/./",
        "/admin/x/..",
        "/admin/x/../",
        "/admin/%2e",
        "/admin/%2e%2e/admin",
        "/x/%2e%2e/admin",
        "/x/%2E%2E/admin",
        "/x/.%2e/admin",
        "/x/%2e./admin",
        "/x/..%2fadmin",
        "/x%2f..%2fadmin",
        "/x/..%5cadmin",
        "/%252e%252e/admin",
        "/admin;a=b",
        "/admin./",
        "/admin%2e",
        "/admin%00.png",
        "/admin%3fx=1",
        "//evil.example/admin",
        "/%61dmin?q=1",
        "/public#/../admin",
        "/admin\\",
        "\\admin",
    ] {
        assert!(refused(spelling), "{spelling:?} slipped past the rule");
    }
    for other in [
        "/",
        "/admin2",
        "/administrator",
        "/adm",
        "/admin/x",
        "/x/admin/y",
        "/.admin",
        "/a%64min%2fx",
        "/admin/x/..%2f..%2f..%2fadmin2",
    ] {
        assert!(!refused(other), "{other:?} is not /admin");
    }
}

fn readings_of(target: &str) -> Vec<CanonicalPath> {
    let target = target.to_owned();
    bounded(move || CanonicalPath::readings(&target))
}

#[test]
fn decoding_stops_for_every_kind_of_escape() {
    // Each of these takes a different branch of the decoder: `%uXXXX`, `%XX` and a byte kept
    // as written, with an invalid escape between them.
    expect("/%u0061b%41c%zz%u00/d", &[&["abac%zz%u00", "d"]]);
    assert_eq!(
        bounded(|| puddle_types::percent_decoded("a%41%u0042%zz%")),
        "aAB%zz%"
    );
    assert_eq!(bounded(|| puddle_types::percent_decoded("%25252541")), "A");
    assert_eq!(
        bounded(|| puddle_types::percent_decoded("%2525252541")),
        "%41"
    );
    assert_eq!(bounded(|| puddle_types::percent_decoded("%2541")), "A");
}

#[test]
fn a_path_pattern_is_at_most_the_limit_long() {
    let at_limit = format!("/{}", "a".repeat(MAX_PATH_PATTERN - 1));
    assert_eq!(at_limit.len(), MAX_PATH_PATTERN);
    assert!(PathPattern::parse(&at_limit).is_ok());
    let over = format!("/{}", "a".repeat(MAX_PATH_PATTERN));
    assert_eq!(PathPattern::parse(&over), Err(PathPatternError::Shape));
}

#[test]
fn a_path_pattern_has_at_most_the_limit_of_segments() {
    let at_limit = "/a".repeat(MAX_PATH_SEGMENTS);
    assert!(PathPattern::parse(&at_limit).is_ok());
    assert_eq!(
        PathPattern::parse(&format!("{at_limit}/a")),
        Err(PathPatternError::TooLong)
    );
    // a wildcard counts as a segment, a closing `**` does not
    let stars = "/*".repeat(MAX_PATH_SEGMENTS);
    assert!(PathPattern::parse(&stars).is_ok());
    assert!(PathPattern::parse(&format!("{stars}/**")).is_ok());
    assert_eq!(
        PathPattern::parse(&format!("{stars}/*")),
        Err(PathPatternError::TooLong)
    );
}

#[test]
fn a_path_pattern_with_a_control_character_or_a_stand_in_is_refused() {
    // Each alone is enough: a control character that is no stand-in, and a stand-in that is no
    // control character.
    for bad in [
        "/a\u{1}",
        "/a\u{0}",
        "/a\n",
        "/a\u{7f}",
        "/a\u{9f}",
        "/\u{e000}",
        "/\u{e001}",
        "/a/\u{e000}/b",
    ] {
        assert_eq!(
            PathPattern::parse(bad),
            Err(PathPatternError::Shape),
            "{bad:?}"
        );
    }
    // the characters around the two stand-ins are ordinary
    assert!(PathPattern::parse("/\u{d7ff}").is_ok());
    assert!(PathPattern::parse("/\u{e002}").is_ok());
    assert!(PathPattern::parse("/\u{a0}").is_ok());
}

#[test]
fn a_method_list_has_at_most_the_limit_of_methods_of_at_most_32_bytes() {
    let names: Vec<String> = (0..MAX_METHODS).map(|i| format!("M{i}")).collect();
    assert_eq!(
        Methods::parse(names.iter().map(String::as_str))
            .unwrap()
            .names()
            .len(),
        MAX_METHODS
    );
    let mut more = names;
    more.push("EXTRA".to_owned());
    assert_eq!(
        Methods::parse(more.iter().map(String::as_str))
            .unwrap_err()
            .to_string(),
        "\"more than 16 methods\" is not an HTTP method"
    );
    let longest = "X".repeat(32);
    assert_eq!(
        Methods::parse([longest.as_str()]).unwrap().names(),
        std::slice::from_ref(&longest)
    );
    assert!(Methods::parse([format!("{longest}X").as_str()]).is_err());
}

#[test]
fn methods_show_as_a_sorted_comma_list_and_none_means_every_method() {
    assert_eq!(Methods::any().to_string(), "");
    assert_eq!(
        Methods::parse(["put", "delete"]).unwrap().to_string(),
        "DELETE,PUT"
    );
    assert_eq!(Methods::parse(["get"]).unwrap().to_string(), "GET");
    assert_eq!(Methods::any().names(), [] as [String; 0]);
    assert!(Methods::any().matches_any(["PATCH"]));
    assert!(!Methods::parse(["get"]).unwrap().matches_any(["PATCH"]));
}

/// A server behind one more decoding hop reads a doubly escaped name as the decoded one; a
/// server that decodes once keeps what is left of it as part of the name.
#[test]
fn a_doubly_escaped_target_is_read_after_one_round_and_after_all_of_them() {
    expect("/x%252fy", &[&["x", "y"], &["x/y"], &["x%2fy"]]);
    expect(
        "/a/b/%252e%252e/c",
        &[&["a", "c"], &["a", "b", "c"], &["a", "b", "%2e%2e", "c"]],
    );
    expect(
        "/a/b%253bx/c",
        &[&["a", "b", "c"], &["a", "b;x", "c"], &["a", "b%3bx", "c"]],
    );
    expect("/a%2525/b", &[&["a%", "b"], &["a%25", "b"]]);
}

/// A target that holds every spelling trick at once still has readings: the choices a server
/// may make are all relevant, and none of them may drop out.
#[test]
fn a_target_with_every_trick_in_it_is_still_read() {
    let target = "/q/../a%252fb;c/d\\e ./%00f";
    let all = readings(target);
    let has = |want: &[&str]| all.iter().any(|reading| reading == want);
    // a lenient server's reading, and the readings that keep one trick each
    assert!(has(&["a", "b", "d", "e", "f"]), "{all:?}");
    assert!(has(&["a%2fb;c", "d\\e ", "f"]), "{all:?}");
    assert!(has(&["a/b", "d", "e", "f"]), "{all:?}");
}
