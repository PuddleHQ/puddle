// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// `blank` under a hang guard. A scanner that stops advancing would otherwise loop and allocate
/// until the process dies of memory, which a mutation run (and anyone breaking the loop) would see
/// as a hang instead of a failed test.
fn blank_bounded(text: &str) -> Result<Blanked, MergeRefusal> {
    let text = text.to_owned();
    puddle_test_kit::returns_within(
        "blank: its scan must always advance",
        std::time::Duration::from_secs(5),
        move || blank(&text),
    )
}

fn view(text: &str) -> String {
    blank_bounded(text).unwrap().view
}

#[test]
fn an_unterminated_string_runs_to_the_end_and_the_parser_refuses_it() {
    let b = blank_bounded("{\"a\": \"open").unwrap();
    assert_eq!(b.view, "{\"a\": \"open");
    assert!(serde_json::from_str::<serde_json::Value>(&b.view).is_err());
}

#[test]
fn comments_become_spaces_and_the_length_is_unchanged() {
    let t = "{ // one\n  \"a\": 1, /* two\nlines */ \"b\": 2 }";
    let b = blank_bounded(t).unwrap();
    assert_eq!(b.view.len(), t.len());
    assert_eq!(b.view, "{       \n  \"a\": 1,       \n         \"b\": 2 }");
    assert_eq!(b.comments, [2..8, 19..34]);
    assert!(serde_json::from_str::<serde_json::Value>(&b.view).is_ok());
}

#[test]
fn comment_lookalikes_inside_strings_stay() {
    let t = r#"{"url": "http://x/*y*/", "e": "\"//"}"#;
    let b = blank(t).unwrap();
    assert_eq!(b.view, t);
    assert_eq!(b.comments.len(), 0);
}

#[test]
fn trailing_commas_are_blanked_only_before_a_closer() {
    assert_eq!(view("{\"a\": [1, 2,],}"), "{\"a\": [1, 2 ] }");
    assert_eq!(view("{\"a\": 1, // c\n}"), "{\"a\": 1      \n}");
    // Not trailing: kept. A doubled comma is left for the JSON parser to reject.
    assert_eq!(view("[1,2]"), "[1,2]");
    let b = blank("[1,,]").unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&b.view).is_err());
}

#[test]
fn line_comments_end_at_either_line_break() {
    let b = blank("{} // a\r\n// b\n").unwrap();
    assert_eq!(b.comments, [3..7, 9..13]);
    assert_eq!(b.view, "{}     \r\n    \n");
}

#[test]
fn multibyte_text_in_comments_keeps_the_length() {
    let t = "{\"é\": 1} // héllo ✓";
    let b = blank(t).unwrap();
    assert_eq!(b.view.len(), t.len());
    assert!(b.view.is_char_boundary(10));
}

#[test]
fn an_unclosed_block_comment_is_a_syntax_error() {
    assert_eq!(
        blank("{} /* nope"),
        Err(MergeRefusal::Syntax {
            format: "JSONC",
            reason: "a comment is never closed".into()
        })
    );
}

#[test]
fn has_comment_in_looks_inside_a_range_only() {
    let b = blank("{ /* c */ }").unwrap();
    assert!(b.has_comment_in(1..10));
    assert!(!b.has_comment_in(0..5));
}
