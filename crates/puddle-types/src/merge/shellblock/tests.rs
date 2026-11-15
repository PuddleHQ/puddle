// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::{MergeFormat, MergeSpec, Merged, unmerge as unmerge_file};

const BODY: &str = "export A='1'\nexport B='two words'";

fn spec(body: &str) -> MergeSpec {
    MergeSpec::new(MergeFormat::ShellBlock, vec![MergeEntry::shell_block(body)]).unwrap()
}

fn ours(body: &str) -> String {
    format!("{BEGIN}\n{body}\n{END}\n")
}

fn written(file: Option<&str>, body: &str) -> String {
    match spec(body).apply(file.map(str::as_bytes), &[]).unwrap() {
        Merged::Write(b) => String::from_utf8(b).unwrap(),
        Merged::Unchanged => file.unwrap().to_owned(),
    }
}

#[test]
fn a_missing_file_becomes_just_the_block() {
    assert_eq!(written(None, BODY), ours(BODY));
}

#[test]
fn a_distro_file_keeps_every_byte_and_gets_the_block_at_its_end() {
    let distro = "# System-wide .bashrc\n[ -z \"$PS1\" ] && return\nPS1='\\u@\\h:\\w\\$ '";
    let out = written(Some(distro), BODY);
    assert_eq!(out, format!("{distro}\n{}", ours(BODY)));
    assert_eq!(
        spec(BODY).apply(Some(out.as_bytes()), &[]).unwrap(),
        Merged::Unchanged
    );
}

#[test]
fn a_changed_value_replaces_the_block_where_it_stands() {
    let first = written(Some("head\n"), BODY);
    let file = format!("{first}tail\n");
    let out = written(Some(&file), "export A='3'");
    assert_eq!(out, format!("head\n{}tail\n", ours("export A='3'")));
}

#[test]
fn removal_takes_out_the_block_and_nothing_else() {
    let file = format!("head\n{}tail\n", ours(BODY));
    let gone = unmerge_file(MergeFormat::ShellBlock, file.as_bytes(), &[]).unwrap();
    assert_eq!(gone.merged, Merged::Write(b"head\ntail\n".to_vec()));
    assert!(!gone.empty);
    let only = unmerge_file(MergeFormat::ShellBlock, ours(BODY).as_bytes(), &[]).unwrap();
    assert!(only.empty);
}

#[test]
fn a_value_cannot_end_the_block_early() {
    let evil = format!("export X='a\n{END}\nb'");
    let err = MergeSpec::new(
        MergeFormat::ShellBlock,
        vec![MergeEntry::shell_block(&evil)],
    )
    .unwrap_err();
    assert!(err.to_string().contains("marker"), "{err}");
}

#[test]
fn an_empty_block_or_one_with_a_nul_byte_is_refused() {
    for bad in ["", "export A='\0'"] {
        assert!(
            MergeSpec::new(MergeFormat::ShellBlock, vec![MergeEntry::shell_block(bad)]).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn a_block_of_exactly_the_limit_is_valid_and_one_byte_more_is_not() {
    let at = "a".repeat(knownhosts::MAX_BLOCK);
    assert_eq!(invalid(&at), None);
    assert!(invalid(&format!("{at}a")).is_some());
}

#[test]
fn the_markers_differ_from_the_known_hosts_ones_so_both_engines_leave_each_other_alone() {
    let hosts = format!(
        "{}\nh ssh-ed25519 AAAA\n{}\n",
        knownhosts::BEGIN,
        knownhosts::END
    );
    let out = written(Some(&hosts), BODY);
    assert!(out.starts_with(&hosts), "{out}");
}

#[test]
fn only_the_puddle_key_is_owned() {
    let other = MergeEntry::json(&["other"], &serde_json::json!("export A=1"));
    let err = MergeSpec::new(MergeFormat::ShellBlock, vec![other]).unwrap_err();
    assert!(err.to_string().contains("only Puddle is owned"), "{err}");
}
