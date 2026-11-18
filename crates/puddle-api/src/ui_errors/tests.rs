// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_store::UserId;

use super::*;
use crate::wire::{UiErrorKind, UiErrorReport};

const API_TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

fn report(message: &str) -> UiErrorReport {
    UiErrorReport {
        kind: UiErrorKind::Uncaught,
        message: message.to_owned(),
        stack: None,
        route: None,
        build: None,
    }
}

#[test]
fn a_url_keeps_its_scheme_host_path_and_place_but_loses_its_user_info_query_and_fragment() {
    assert_eq!(
        scrub_line("GET https://me:hunter2@git.example.com/a/b?access_token=xyz#k failed"),
        "GET https://git.example.com/a/b failed"
    );
    assert_eq!(
        scrub_line(
            "    at load (http://127.0.0.1:4100/_app/immutable/nodes/31.C-HWBeUv.js:9:7650)"
        ),
        "    at load (http://127.0.0.1:4100/_app/immutable/nodes/31.C-HWBeUv.js:9:7650)"
    );
    // The line and column after a query stay: they say where, and hold no secret.
    assert_eq!(
        scrub_line("at f (http://localhost:5173/src/x.ts?t=1700000000:4:5)"),
        "at f (http://localhost:5173/src/x.ts:4:5)"
    );
    // A query that ends in a colon and no number keeps nothing of it.
    assert_eq!(
        scrub_line("x (https://h.example/a.js?k=a:b)"),
        "x (https://h.example/a.js)"
    );
    assert_eq!(
        scrub_line("see wss://u:p@h.example/ws?ticket=abc"),
        "see wss://h.example/ws"
    );
}

#[test]
fn tokens_are_replaced_wherever_they_stand() {
    for (text, want) in [
        (
            format!("Authorization: Bearer {API_TOKEN}"),
            "Authorization: Bearer [redacted]".to_owned(),
        ),
        (
            format!("bad token {API_TOKEN}."),
            "bad token [redacted].".to_owned(),
        ),
        (
            "basic dXNlcjpwYXNzd29yZA== was refused".to_owned(),
            "basic [redacted] was refused".to_owned(),
        ),
        (
            "push with ghp_abcdefghijklmnopqrstuvwxyz0123456789 failed".to_owned(),
            "push with [redacted] failed".to_owned(),
        ),
        (
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl here".to_owned(),
            "jwt [redacted] here".to_owned(),
        ),
        (
            "body password=hunter2&user=me".to_owned(),
            "body password=[redacted]".to_owned(),
        ),
        (
            "\"token\":\"short\" in JSON".to_owned(),
            "\"token\":[redacted] in JSON".to_owned(),
        ),
        (
            "set api_key: abc".to_owned(),
            "set api_key: [redacted]".to_owned(),
        ),
    ] {
        assert_eq!(scrub_line(&text), want, "{text}");
    }
}

#[test]
fn ordinary_text_and_stack_frames_pass_unchanged() {
    for text in [
        "TypeError: Cannot assign to read only property 'toString' of object '#<Object>'",
        "    at Object.render (/_app/immutable/chunks/Bp3kq9xZ.js:1:2345)",
        "ReferenceError: keyboardLayout is not defined",
        "the workspace qa-first stopped",
        "    at async Promise.all (index 0)",
    ] {
        assert_eq!(scrub_line(text), text);
    }
}

#[test]
fn text_is_bounded_in_lines_and_characters_and_loses_control_characters() {
    let many = (0..100)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let kept = scrub(&many, 10_000, 3);
    assert_eq!(kept, "line 0\nline 1\nline 2\n…");
    let long = "x".repeat(5000);
    let kept = scrub(&long, 100, 3);
    assert_eq!(kept.chars().count(), 101);
    assert!(kept.ends_with('…'));
    assert_eq!(scrub("a\u{1b}[31mb\r\nc\td", 100, 5), "a [31mb\nc d");
}

#[test]
fn a_route_is_a_path_alone() {
    assert_eq!(
        route("/invite?code=abc#token=x"),
        Some("/invite".to_owned())
    );
    assert_eq!(route("  "), None);
    assert_eq!(route("/a\nb"), Some("/a b".to_owned()));
    assert_eq!(
        route(&format!("/x/{}", "y".repeat(500))).map(|r| r.chars().count()),
        Some(ROUTE_CHARS + 1)
    );
}

#[test]
fn a_report_is_kept_scrubbed_and_listed_for_its_user_only() {
    let book = UiErrors::default();
    let me = UserId::OWNER;
    let mut sent = report(&format!("TypeError: token {API_TOKEN} broke"));
    sent.stack = Some(format!(
        "TypeError: x\n    at f (https://u:p@h.example/a.js?k=v:1:2)\n{API_TOKEN}"
    ));
    sent.route = Some("/workspaces/abc/terminal?x=1".into());
    sent.build = Some("1700000000000".into());
    let Outcome::Kept(kept) = book.report(me, &sent, 5) else {
        panic!("refused")
    };
    assert_eq!(kept.message, "TypeError: token [redacted] broke");
    assert_eq!(
        kept.stack.as_deref(),
        Some("TypeError: x\n    at f (https://h.example/a.js:1:2)\n[redacted]")
    );
    assert_eq!(kept.route.as_deref(), Some("/workspaces/abc/terminal"));
    assert_eq!(kept.at, 5);
    let list = book.list(me);
    assert_eq!(list.errors, vec![kept]);
    assert_eq!(book.list(UserId::from_stored(2)).errors, vec![]);
}

#[test]
fn past_the_rate_reports_are_refused_and_counted_until_the_window_moves() {
    let book = UiErrors::default();
    let me = UserId::OWNER;
    for i in 0..PER_WINDOW {
        assert!(
            matches!(
                book.report(me, &report(&format!("e{i}")), 1_000),
                Outcome::Kept(_)
            ),
            "{i}"
        );
    }
    assert_eq!(
        book.report(me, &report("over"), 1_000),
        Outcome::Refused {
            retry_after_s: 60,
            first: true
        }
    );
    assert_eq!(
        book.report(me, &report("over"), 30_999),
        Outcome::Refused {
            retry_after_s: 31,
            first: false
        }
    );
    // Another user has a rate of their own.
    assert!(matches!(
        book.report(UserId::from_stored(2), &report("theirs"), 1_000),
        Outcome::Kept(_)
    ));
    assert_eq!(book.list(me).dropped, 2);
    // A window later the next one is taken again.
    assert!(matches!(
        book.report(me, &report("later"), 1_000 + WINDOW_MS),
        Outcome::Kept(_)
    ));
}

#[test]
fn only_the_most_recent_are_kept_newest_first() {
    let book = UiErrors::default();
    let me = UserId::OWNER;
    let total = u64::try_from(KEEP).unwrap() + 5;
    for i in 0..total {
        // Spread out so the rate never refuses.
        let at = i * WINDOW_MS;
        assert!(matches!(
            book.report(me, &report(&format!("e{i}")), at),
            Outcome::Kept(_)
        ));
    }
    let list = book.list(me);
    assert_eq!(list.errors.len(), KEEP);
    assert_eq!(list.errors[0].message, format!("e{}", total - 1));
    assert_eq!(list.errors[KEEP - 1].message, "e5");
}

#[test]
fn a_url_after_a_character_of_several_bytes_is_scrubbed_without_a_panic() {
    assert_eq!(
        scrub_line("failed “https://u:p@x.example/y?k=v” and →https://x.example/z#f"),
        "failed “https://x.example/y” and →https://x.example/z"
    );
}

#[test]
fn a_token_in_a_path_is_replaced_and_the_rest_of_the_path_stays() {
    assert_eq!(
        scrub_line(&format!(
            "GET https://h.example/reset/{API_TOKEN}/done failed"
        )),
        "GET https://h.example/reset/[redacted]/done failed"
    );
    assert_eq!(
        route(&format!("/invite/{API_TOKEN}")),
        Some("/invite/[redacted]".to_owned())
    );
}

#[test]
fn a_secret_name_and_its_value_with_spaces_between_lose_the_value() {
    assert_eq!(scrub_line("password = hunter2"), "password = [redacted]");
    assert_eq!(scrub_line("accessToken : abc"), "accessToken : [redacted]");
    assert_eq!(scrub_line("password =hunter2"), "password =[redacted]");
}

#[test]
fn only_whole_name_parts_say_secret() {
    for text in [
        "author: Ann",
        "keyboard=dvorak",
        "monkey: banana",
        "authenticated: yes",
    ] {
        assert_eq!(scrub_line(text), text);
    }
    for (text, want) in [
        ("accessToken: abc", "accessToken: [redacted]"),
        ("X-Auth-Token: abc", "X-Auth-Token: [redacted]"),
        ("sessionId=abc", "sessionId=[redacted]"),
        ("API_KEY=abc", "API_KEY=[redacted]"),
        ("apikey=abc", "apikey=[redacted]"),
        ("APIKey=abc", "APIKey=[redacted]"),
        ("accesstoken: abc", "accesstoken: [redacted]"),
        ("sessionid=abc", "sessionid=[redacted]"),
        ("csrftoken=abc", "csrftoken=[redacted]"),
    ] {
        assert_eq!(scrub_line(text), want, "{text}");
    }
}

#[test]
fn line_separators_and_direction_marks_become_spaces() {
    assert_eq!(
        scrub(
            "a\u{2028}b\u{202e}c\u{2066}d\u{200f}e\u{200b}f\u{061c}g",
            100,
            5
        ),
        "a b c d e f g"
    );
}

#[test]
fn a_user_whose_window_ended_is_forgotten() {
    let book = UiErrors::default();
    assert!(matches!(
        book.report(UserId::OWNER, &report("e"), 0),
        Outcome::Kept(_)
    ));
    assert!(matches!(
        book.report(UserId::from_stored(2), &report("e"), WINDOW_MS),
        Outcome::Kept(_)
    ));
    let inner = book.inner.lock().unwrap();
    assert_eq!(
        inner.taken.keys().copied().collect::<Vec<_>>(),
        vec![UserId::from_stored(2)]
    );
}
