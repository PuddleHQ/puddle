// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

fn spec(entries: Vec<MergeEntry>) -> MergeSpec {
    MergeSpec::new(MergeFormat::Json, entries).unwrap()
}

#[test]
fn spec_refuses_empty_overlapping_and_invalid_entries() {
    let err = |entries| {
        MergeSpec::new(MergeFormat::Json, entries)
            .unwrap_err()
            .to_string()
    };
    assert!(err(vec![]).contains("at least one key"));
    assert!(err(vec![MergeEntry::json(&[], &json!(1))]).contains("key path is empty"));
    assert!(
        err(vec![
            MergeEntry::json(&["a", "b"], &json!(1)),
            MergeEntry::json(&["a"], &json!(1)),
        ])
        .contains("\"a.b\": overlaps")
    );
    assert!(
        err(vec![
            MergeEntry::json(&["a"], &json!(1)),
            MergeEntry::json(&["a"], &json!(2)),
        ])
        .contains("overlaps")
    );
    let bad = MergeEntry {
        key: vec!["a".into()],
        value: "{".into(),
    };
    assert!(err(vec![bad]).contains("not JSON"));
    // Siblings and keys that only share a string prefix are fine.
    spec(vec![
        MergeEntry::json(&["a", "b"], &json!(1)),
        MergeEntry::json(&["a", "c"], &json!(1)),
        MergeEntry::json(&["ab"], &json!(1)),
    ]);
}

#[test]
fn spec_round_trips_through_serde_and_validates_on_the_way_in() {
    let s = spec(vec![MergeEntry::json(&["k"], &json!({"x": [1, 2]}))]);
    let text = serde_json::to_string(&s).unwrap();
    assert_eq!(
        text,
        r#"{"format":"json","entries":[{"key":["k"],"value":"{\"x\":[1,2]}"}]}"#
    );
    assert_eq!(serde_json::from_str::<MergeSpec>(&text).unwrap(), s);
    assert!(serde_json::from_str::<MergeSpec>(r#"{"format":"json","entries":[]}"#).is_err());
    assert_eq!(s.format(), MergeFormat::Json);
    assert_eq!(s.entries()[0].key(), ["k"]);
    assert_eq!(s.entries()[0].value(), r#"{"x":[1,2]}"#);
}

#[test]
fn fresh_file_is_tab_indented_like_the_docker_cli_writes_it() {
    let s = spec(vec![MergeEntry::json(
        &["proxies", "default"],
        &json!({"httpProxy": "http://172.17.0.1:3128", "noProxy": "localhost"}),
    )]);
    assert_eq!(
        String::from_utf8(s.fresh()).unwrap(),
        "{\n\t\"proxies\": {\n\t\t\"default\": {\n\t\t\t\"httpProxy\": \"http://172.17.0.1:3128\",\n\t\t\t\"noProxy\": \"localhost\"\n\t\t}\n\t}\n}\n"
    );
    // An empty or whitespace-only file counts as no file.
    assert_eq!(
        s.apply(Some(b" \n"), &[]).unwrap(),
        Merged::Write(s.fresh())
    );
}

#[test]
fn unreadable_files_are_refused_with_a_reason_and_no_content() {
    let s = spec(vec![MergeEntry::json(&["a"], &json!(1))]);
    let big = vec![b' '; MAX_MERGE_FILE + 1];
    assert_eq!(s.apply(Some(&big), &[]), Err(MergeRefusal::TooLarge));
    let e = s.apply(Some(b"\xff{}"), &[]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "the file is not valid UTF-8: invalid byte sequence"
    );
    let e = s.apply(Some(b"{\"secret-token\": "), &[]).unwrap_err();
    assert!(matches!(e, MergeRefusal::Syntax { format: "JSON", .. }));
    assert!(!e.to_string().contains("secret-token"), "{e}");
    assert_eq!(s.apply(Some(b"[1]"), &[]), Err(MergeRefusal::NotAnObject));
    assert_eq!(
        unmerge(MergeFormat::Json, b"nope", &[vec!["a".into()]]).unwrap_err(),
        MergeRefusal::Syntax {
            format: "JSON",
            reason: "expected ident at line 1 column 2".into()
        }
    );
}

#[test]
fn unmerge_of_an_empty_file_is_a_no_op() {
    let u = unmerge(MergeFormat::Json, b"", &[vec!["a".into()]]).unwrap();
    assert_eq!(u.merged, Merged::Unchanged);
    assert!(u.empty);
}
