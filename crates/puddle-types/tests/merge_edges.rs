// SPDX-License-Identifier: GPL-3.0-or-later
//! Edges of the merge engine that the unit tests next to the code leave open: the exact file size
//! limit for every format, a lone `/` outside a string in a commented JSON file, and the scanner
//! of commented JSON (commas, closing brackets, strings with escapes) running to the end of its
//! input. Each assertion is the result a caller sees at the public API.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a helper outside #[test] fails the test by panicking"
)]

use std::sync::mpsc;
use std::time::Duration;

use puddle_types::{
    MAX_MERGE_FILE, MergeEntry, MergeFormat, MergeRefusal, MergeSpec, Merged, unmerge,
};
use serde_json::json;

/// Runs `work` on its own thread and fails the test when it has not finished in five seconds, so
/// a scanner that stops advancing is a failed test and not a hang.
fn bounded<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the work did not finish in five seconds")
}

fn json_spec(format: MergeFormat) -> MergeSpec {
    MergeSpec::new(format, vec![MergeEntry::json(&["zz"], &json!(1))]).unwrap()
}

fn ssh_spec() -> MergeSpec {
    MergeSpec::new(
        MergeFormat::SshConfig,
        vec![MergeEntry::ssh_include("/etc/ssh/ssh_config.d/*.conf")],
    )
    .unwrap()
}

/// A JSON object of exactly `len` bytes.
fn json_of(len: usize) -> Vec<u8> {
    let fill = len - r#"{"a":""}"#.len();
    format!(r#"{{"a":"{}"}}"#, "x".repeat(fill)).into_bytes()
}

/// A configuration file of exactly `len` bytes made of comment lines.
fn ssh_of(len: usize) -> Vec<u8> {
    let mut text = String::new();
    while text.len() + 2 < len {
        text.push_str("#\n");
    }
    text.push_str(&"#".repeat(len - text.len()));
    assert_eq!(text.len(), len);
    text.into_bytes()
}

#[test]
fn a_json_file_of_exactly_the_limit_is_merged_and_one_byte_more_is_refused() {
    for format in [MergeFormat::Json, MergeFormat::Jsonc] {
        let spec = json_spec(format);
        let ok = json_of(MAX_MERGE_FILE);
        assert_eq!(ok.len(), MAX_MERGE_FILE);
        assert!(matches!(
            spec.apply(Some(&ok), &[]).unwrap(),
            Merged::Write(_)
        ));
        let unmerged = unmerge(format, &ok, &[vec!["a".into()]]).unwrap();
        assert!(matches!(unmerged.merged, Merged::Write(_)));

        let big = json_of(MAX_MERGE_FILE + 1);
        assert_eq!(
            spec.apply(Some(&big), &[]).unwrap_err(),
            MergeRefusal::TooLarge
        );
        assert_eq!(
            unmerge(format, &big, &[vec!["a".into()]]).unwrap_err(),
            MergeRefusal::TooLarge
        );
    }
}

#[test]
fn an_ssh_config_of_exactly_the_limit_is_merged_and_one_byte_more_is_refused() {
    let ok = ssh_of(MAX_MERGE_FILE);
    assert!(matches!(
        ssh_spec().apply(Some(&ok), &[]).unwrap(),
        Merged::Write(_)
    ));
    assert!(unmerge(MergeFormat::SshConfig, &ok, &[]).is_ok());

    let big = ssh_of(MAX_MERGE_FILE + 1);
    assert_eq!(
        ssh_spec().apply(Some(&big), &[]).unwrap_err(),
        MergeRefusal::TooLarge
    );
    assert_eq!(
        unmerge(MergeFormat::SshConfig, &big, &[]).unwrap_err(),
        MergeRefusal::TooLarge
    );
}

#[test]
fn a_lone_slash_outside_a_string_is_a_syntax_error_even_when_a_block_comment_closes_later() {
    let text = b"{\"a\": 1 /x */ }";
    let err = json_spec(MergeFormat::Jsonc)
        .apply(Some(text), &[])
        .unwrap_err();
    assert!(matches!(err, MergeRefusal::Syntax { .. }), "{err:?}");
}

#[test]
fn commented_json_with_commas_brackets_whitespace_and_strings_is_scanned_to_its_end() {
    let text = "{\n\t\"a\": [1, 2, 3,], // note\r\n  \"b\": {\"c\": \"d\\\\\", \"e\": \"f\\\"//g\",},\n  /* block */ \"h\": \"//i\" ,\n}\n";
    let out = bounded(move || {
        json_spec(MergeFormat::Jsonc)
            .apply(Some(text.as_bytes()), &[])
            .unwrap()
    });
    let Merged::Write(bytes) = out else {
        panic!("the file lacks the key, so it is written");
    };
    let out = String::from_utf8(bytes).unwrap();
    // Every comment and every trailing comma is kept, and the key is added.
    for kept in [
        "// note\r\n",
        "/* block */",
        "[1, 2, 3,]",
        "\"d\\\\\", \"e\": \"f\\\"//g\",}",
        "\"//i\" ,",
    ] {
        assert!(out.contains(kept), "{kept:?} missing from {out:?}");
    }
    assert!(out.contains("\"zz\""), "{out:?}");
}

#[test]
fn a_string_with_an_escaped_quote_ends_at_the_next_quote_whatever_its_offset() {
    for pad in 0..6 {
        let text = format!(
            "{{{}\"k\": \"a\\\"b/*c\", /* x */ \"l\": 1}}",
            " ".repeat(pad)
        );
        let out = bounded(move || {
            json_spec(MergeFormat::Jsonc)
                .apply(Some(text.as_bytes()), &[])
                .unwrap()
        });
        let Merged::Write(bytes) = out else {
            panic!("written");
        };
        let out = String::from_utf8(bytes).unwrap();
        assert!(out.contains("\"a\\\"b/*c\", /* x */"), "{out:?}");
    }
}

#[test]
fn an_unterminated_string_is_a_syntax_error_and_ends_the_scan() {
    for text in ["{\"a\": \"x", "{\"a\": \"x\\", "{\"a\": \"x\\\""] {
        let err = bounded(move || {
            json_spec(MergeFormat::Jsonc)
                .apply(Some(text.as_bytes()), &[])
                .unwrap_err()
        });
        assert!(
            matches!(err, MergeRefusal::Syntax { .. }),
            "{text:?}: {err:?}"
        );
    }
}

#[test]
fn a_member_is_added_behind_a_block_comment_that_spans_the_end_of_the_last_line() {
    let text = "{\n  \"a\": 1, /* first\n  second */\n}\n";
    let out = json_spec(MergeFormat::Jsonc)
        .apply(Some(text.as_bytes()), &[])
        .unwrap();
    // The member goes right behind the value: the line break inside the comment is not a place to
    // split, and the comment stays in one piece.
    assert_eq!(
        out,
        Merged::Write(
            "{\n  \"a\": 1,\n  \"zz\": 1, /* first\n  second */\n}\n"
                .as_bytes()
                .to_vec()
        )
    );
}

#[test]
fn a_member_goes_on_the_line_after_the_last_one_when_only_a_line_comment_follows() {
    let text = "{\n  \"a\": 1 // keep\n}\n";
    let out = json_spec(MergeFormat::Jsonc)
        .apply(Some(text.as_bytes()), &[])
        .unwrap();
    assert_eq!(
        out,
        Merged::Write(
            "{\n  \"a\": 1, // keep\n  \"zz\": 1\n}\n"
                .as_bytes()
                .to_vec()
        )
    );
}

#[test]
fn removing_a_first_member_keeps_a_comment_behind_it_and_the_indent_of_the_next_one() {
    for indent in [" ", "  ", "\t", "\t\t", " \t"] {
        let text = format!("{{\n{indent}\"a\": 1, // note\n{indent}\"b\": 2\n}}\n");
        let out = unmerge(MergeFormat::Jsonc, text.as_bytes(), &[vec!["a".into()]]).unwrap();
        let expected = format!("{{\n{indent}// note\n{indent}\"b\": 2\n}}\n");
        assert_eq!(
            out.merged,
            Merged::Write(expected.into_bytes()),
            "indent {indent:?}"
        );
        assert!(!out.empty);
    }
}
