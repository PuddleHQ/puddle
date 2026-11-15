// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::MAX_MERGE_FILE;
use crate::{MergeFormat, MergeSpec};

const WANTED: &str = "/etc/ssh/ssh_config.d/*.conf";

fn spec() -> MergeSpec {
    MergeSpec::new(
        MergeFormat::SshConfig,
        vec![MergeEntry::ssh_include(WANTED)],
    )
    .unwrap()
}

fn line() -> String {
    format!("{MARKER}\nInclude {WANTED}\n")
}

fn applied(file: &str) -> Merged {
    spec().apply(Some(file.as_bytes()), &[]).unwrap()
}

/// Debian's file: the line is there, so the file is not rewritten.
const DEBIAN: &str = "\nInclude /etc/ssh/ssh_config.d/*.conf\n\nHost *\n    SendEnv LANG LC_*\n";

#[test]
fn a_file_without_the_line_gets_it_on_top_and_keeps_every_other_byte() {
    for file in [
        "Host *\n    SendEnv LANG LC_*\n",
        "# comment\r\nHost *\r\n\tForwardAgent no",
        "  \n\n",
        "Match host x\n  User y\n",
        "# Include /etc/ssh/ssh_config.d/*.conf\n",
        "Host a\n    Include /etc/ssh/ssh_config.d/*.conf\n",
        "Host *\n  # Include /etc/ssh/ssh_config.d/*.conf\n  Port=22\n",
        "Include /etc/ssh/other.d/*.conf\n",
        "Port 22\nHost x\n  Include /etc/ssh/ssh_config.d/*.conf\n",
    ] {
        let expected = Merged::Write(format!("{}{file}", line()).into_bytes());
        assert_eq!(applied(file), expected, "{file:?}");
    }
}

#[test]
fn a_missing_or_empty_file_becomes_just_the_line() {
    let fresh = spec().fresh();
    assert_eq!(String::from_utf8(fresh.clone()).unwrap(), line());
    assert_eq!(
        spec().apply(None, &[]).unwrap(),
        Merged::Write(fresh.clone())
    );
    assert_eq!(spec().apply(Some(b""), &[]).unwrap(), Merged::Write(fresh));
}

#[test]
fn an_include_that_reaches_the_folder_leaves_the_file_alone() {
    for file in [
        DEBIAN,
        "Include /etc/ssh/ssh_config.d/*.conf",
        "# top\ninclude=/etc/ssh/ssh_config.d/*.conf\nHost *\n",
        "Include ssh_config.d/*.conf\n",
        "Include \"/etc/ssh/ssh_config.d/*.conf\"\n",
        "Include /etc/ssh/ssh_config.d/*\n",
        "Include /nowhere/*.conf /etc/ssh/ssh_config.d/*.conf # both\n",
        "Include\t/etc/ssh/ssh_config.d/*.conf\n",
    ] {
        assert_eq!(applied(file), Merged::Unchanged, "{file:?}");
    }
}

#[test]
fn applying_twice_changes_nothing_the_second_time() {
    let first = format!("{}Host *\n    Port 22\n", line());
    assert_eq!(
        applied("Host *\n    Port 22\n"),
        Merged::Write(first.clone().into_bytes())
    );
    assert_eq!(applied(&first), Merged::Unchanged);
}

#[test]
fn an_unreadable_file_is_refused_and_not_quoted() {
    let s = spec();
    assert_eq!(
        s.apply(Some(&vec![b' '; MAX_MERGE_FILE + 1]), &[]),
        Err(MergeRefusal::TooLarge)
    );
    assert_eq!(
        s.apply(Some(b"Host a\0\n"), &[]),
        Err(MergeRefusal::NulByte)
    );
    let e = s.apply(Some(b"Host \xff\n"), &[]).unwrap_err();
    assert_eq!(
        e.to_string(),
        "the file is not valid UTF-8: invalid byte sequence"
    );
    assert_eq!(
        crate::unmerge(MergeFormat::SshConfig, b"\xff", &[vec!["Include".into()]]),
        Err(e)
    );
}

#[test]
fn removal_takes_out_exactly_the_pair_puddle_added() {
    let keys = [vec!["Include".to_owned()]];
    let user = "Host *\n    Port 22\n";
    let unmerge =
        |file: &str| crate::unmerge(MergeFormat::SshConfig, file.as_bytes(), &keys).unwrap();
    let back = unmerge(&format!("{}{user}", line()));
    assert_eq!(back.merged, Merged::Write(user.as_bytes().to_vec()));
    assert!(!back.empty);
    // A file Puddle created is empty once the pair is gone.
    let alone = unmerge(&line());
    assert!(alone.empty);
    assert_eq!(alone.merged, Merged::Write(Vec::new()));
    // The image's own line, an edited pair and a moved line are not Puddle's.
    for file in [DEBIAN, "Host *\nInclude /etc/ssh/ssh_config.d/*.conf\n"] {
        let u = unmerge(file);
        assert_eq!(u.merged, Merged::Unchanged, "{file:?}");
        assert!(!u.empty);
    }
    let edited = format!("{MARKER}\nHost x\nInclude {WANTED}\n");
    assert_eq!(unmerge(&edited).merged, Merged::Unchanged);
    assert_eq!(unmerge(MARKER).merged, Merged::Unchanged);
    assert!(unmerge("  \n").empty);
}

#[test]
fn removal_after_the_user_added_lines_below_keeps_them() {
    let file = format!("{}Host a\r\n  User b\r\n", line());
    let u = crate::unmerge(
        MergeFormat::SshConfig,
        file.as_bytes(),
        &[vec!["Include".into()]],
    )
    .unwrap();
    assert_eq!(u.merged, Merged::Write(b"Host a\r\n  User b\r\n".to_vec()));
}

#[test]
fn the_spec_owns_only_one_include_word() {
    let bad = |entry| {
        MergeSpec::new(MergeFormat::SshConfig, vec![entry])
            .unwrap_err()
            .to_string()
    };
    assert!(bad(MergeEntry::json(&["Host"], &serde_json::json!("x"))).contains("only Include"));
    for value in ["", "a b", "a\"b", "a#b", "a\nb"] {
        assert!(
            bad(MergeEntry::ssh_include(value)).contains("one word"),
            "{value:?}"
        );
    }
}
