// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::MAX_MERGE_FILE;
use crate::{MergeFormat, MergeSpec};

const KEY_A: &str = "github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAA";
const KEY_B: &str = "[github.com]:443 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAA";

fn spec(body: &str) -> MergeSpec {
    MergeSpec::new(MergeFormat::KnownHosts, vec![MergeEntry::known_hosts(body)]).unwrap()
}

fn ours(body: &str) -> String {
    format!("{BEGIN}\n{body}\n{END}\n")
}

fn applied(body: &str, file: &str) -> Merged {
    spec(body).apply(Some(file.as_bytes()), &[]).unwrap()
}

fn written(body: &str, file: &str) -> String {
    match applied(body, file) {
        Merged::Write(bytes) => String::from_utf8(bytes).unwrap(),
        Merged::Unchanged => file.to_owned(),
    }
}

#[test]
fn a_missing_or_empty_file_becomes_just_the_block() {
    let fresh = spec(KEY_A).fresh();
    assert_eq!(String::from_utf8(fresh.clone()).unwrap(), ours(KEY_A));
    assert_eq!(
        spec(KEY_A).apply(None, &[]).unwrap(),
        Merged::Write(fresh.clone())
    );
    assert_eq!(
        spec(KEY_A).apply(Some(b""), &[]).unwrap(),
        Merged::Write(fresh)
    );
}

#[test]
fn a_file_without_markers_gets_the_block_at_its_end_and_keeps_every_byte() {
    let user = "example.org ssh-rsa AAAA\r\n# mine\n|1|salt|hash ssh-rsa BBBB\n";
    assert_eq!(written(KEY_A, user), format!("{user}{}", ours(KEY_A)));
    // A last line with no line break gets one first, so the lines stay apart.
    assert_eq!(
        written(KEY_A, "example.org ssh-rsa AAAA"),
        format!("example.org ssh-rsa AAAA\n{}", ours(KEY_A))
    );
}

#[test]
fn a_block_of_exactly_the_limit_is_valid_and_one_byte_more_is_not() {
    assert_eq!(invalid(&"k".repeat(MAX_BLOCK)), None);
    assert_eq!(
        invalid(&"k".repeat(MAX_BLOCK + 1)),
        Some("the block is too large")
    );
}

#[test]
fn a_block_in_place_is_replaced_where_it_stands() {
    let file = format!("top ssh-rsa A\n{}bottom ssh-rsa B\n", ours("old ssh-rsa C"));
    let expected = format!("top ssh-rsa A\n{}bottom ssh-rsa B\n", ours(KEY_A));
    assert_eq!(written(KEY_A, &file), expected);
    // The same keys change nothing, not even the bytes.
    assert_eq!(applied(KEY_A, &expected), Merged::Unchanged);
    assert_eq!(written(KEY_A, &expected), expected);
}

#[test]
fn two_lines_in_a_block_stay_two_lines() {
    let body = format!("{KEY_A}\n{KEY_B}");
    let once = written(&body, "x ssh-rsa A\n");
    assert_eq!(once, format!("x ssh-rsa A\n{}", ours(&body)));
    assert_eq!(applied(&body, &once), Merged::Unchanged);
}

#[test]
fn crlf_markers_are_still_puddles() {
    let file = format!("{BEGIN}\r\nold ssh-rsa C\r\n{END}\r\nuser ssh-rsa D\r\n");
    assert_eq!(
        written(KEY_A, &file),
        format!("{}user ssh-rsa D\r\n", ours(KEY_A))
    );
}

#[test]
fn damaged_markers_are_repaired_by_dropping_the_span_and_writing_the_block_again() {
    let two = format!(
        "a ssh-rsa A\n{}between ssh-rsa M\n{}z ssh-rsa Z\n",
        ours("one ssh-rsa 1"),
        ours("two ssh-rsa 2")
    );
    for (file, left) in [
        // the end marker is gone: only the begin line goes
        (
            format!("{BEGIN}\nold ssh-rsa C\nuser ssh-rsa U\n"),
            "old ssh-rsa C\nuser ssh-rsa U\n",
        ),
        // the begin marker is gone
        (
            format!("user ssh-rsa U\nold ssh-rsa C\n{END}\n"),
            "user ssh-rsa U\nold ssh-rsa C\n",
        ),
        // the end comes before the begin: everything between and including goes
        (
            format!("a ssh-rsa A\n{END}\nmid ssh-rsa M\n{BEGIN}\nz ssh-rsa Z\n"),
            "a ssh-rsa A\nz ssh-rsa Z\n",
        ),
        // a duplicated block: from the first marker to the last one
        (two, "a ssh-rsa A\nz ssh-rsa Z\n"),
        // a marker inside its own block
        (format!("{BEGIN}\n{BEGIN}\nold ssh-rsa C\n{END}\n"), ""),
    ] {
        assert_eq!(
            written(KEY_A, &file),
            format!("{left}{}", ours(KEY_A)),
            "{file:?}"
        );
    }
}

#[test]
fn the_markers_must_match_the_whole_line() {
    let file = format!("{BEGIN} not ours\nx ssh-rsa A\n");
    assert_eq!(written(KEY_A, &file), format!("{file}{}", ours(KEY_A)));
}

#[test]
fn an_unreadable_file_is_refused_and_not_quoted() {
    let s = spec(KEY_A);
    assert_eq!(
        s.apply(Some(&vec![b'#'; MAX_MERGE_FILE + 1]), &[]),
        Err(MergeRefusal::TooLarge)
    );
    assert_eq!(s.apply(Some(b"a\0b\n"), &[]), Err(MergeRefusal::NulByte));
    let e = s.apply(Some(b"a \xff\n"), &[]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "the file is not valid UTF-8: invalid byte sequence"
    );
    assert_eq!(
        crate::unmerge(MergeFormat::KnownHosts, b"\xff", &[vec![KEY.into()]]),
        Err(e)
    );
}

fn removed(file: &str) -> Unmerged {
    crate::unmerge(
        MergeFormat::KnownHosts,
        file.as_bytes(),
        &[vec![KEY.to_owned()]],
    )
    .unwrap()
}

#[test]
fn removal_takes_out_exactly_the_block() {
    let user = "a ssh-rsa A\nb ssh-rsa B\n";
    let back = removed(&format!("a ssh-rsa A\n{}b ssh-rsa B\n", ours(KEY_A)));
    assert_eq!(back.merged, Merged::Write(user.as_bytes().to_vec()));
    assert!(!back.empty);
    let alone = removed(&ours(KEY_A));
    assert!(alone.empty);
    assert_eq!(alone.merged, Merged::Write(Vec::new()));
    let none = removed(user);
    assert_eq!(none.merged, Merged::Unchanged);
    assert!(!none.empty);
    assert!(removed("  \n").empty);
    // A damaged block goes the way the repair drops it.
    let damaged = removed(&format!("{BEGIN}\nold ssh-rsa C\n"));
    assert_eq!(damaged.merged, Merged::Write(b"old ssh-rsa C\n".to_vec()));
}

#[test]
fn the_spec_owns_one_block_of_host_key_lines() {
    let bad = |entries| {
        MergeSpec::new(MergeFormat::KnownHosts, entries)
            .unwrap_err()
            .to_string()
    };
    let other = MergeEntry::json(&["Host"], &serde_json::json!("x"));
    assert!(bad(vec![other]).contains("only Puddle"));
    for (value, why) in [
        ("", "empty"),
        ("a ssh-rsa A\n\nb ssh-rsa B", "host-key entry"),
        ("a ssh-rsa A\n", "host-key entry"),
        ("# note", "host-key entry"),
        ("a ssh-rsa A\u{0}", "control character"),
        ("a ssh-rsa A\r\n", "control character"),
    ] {
        assert!(
            bad(vec![MergeEntry::known_hosts(value)]).contains(why),
            "{value:?}"
        );
    }
    let big = "a ".repeat(MAX_BLOCK);
    assert!(bad(vec![MergeEntry::known_hosts(&big)]).contains("too large"));
}
