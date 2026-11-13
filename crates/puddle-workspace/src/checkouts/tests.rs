// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt::Write as _;

use super::*;

#[test]
fn a_full_listing_parses() {
    let out = "C\tapi\thttps://github.com/o/api.git\tmain\tabc1234\t0\n\
                   C\tweb\thttps://u:tok@host/o/web\t\tdef5678\t1\n\
                   C\tfresh\t\tmaster\t\t0\n\
                   C\tssh\tgit@github.com:o/r.git\tmain\tabc\t0\n\
                   C\tlocal\t/srv/repo\tmain\tabc\t0\n\
                   C\tplain\thttp://host/r\tmain\tabc\t0\n\
                   E\tbroken\tgit remote failed: boom\n\
                   M\t7\nO\t3\nD\n";
    let list = parse(out).unwrap();
    assert_eq!((list.more, list.others), (7, 3));
    let by = |dir: &str| list.checkouts.iter().find(|c| c.dir == dir).unwrap();
    let api = by("api");
    assert_eq!(
        api.remote_url.as_deref(),
        Some("https://github.com/o/api.git")
    );
    assert_eq!(api.remote_note, None);
    assert_eq!(api.branch.as_deref(), Some("main"));
    assert_eq!(api.head.as_deref(), Some("abc1234"));
    assert!(!api.dirty);
    let web = by("web");
    assert_eq!(web.remote_url.as_deref(), Some("https://host/o/web"));
    assert_eq!(web.remote_note, Some(RemoteNote::CredentialsRemoved));
    assert_eq!(web.branch, None, "detached");
    assert!(web.dirty);
    let fresh = by("fresh");
    assert_eq!(
        (fresh.remote_url.as_deref(), fresh.head.as_deref()),
        (None, None)
    );
    assert_eq!(fresh.remote_note, Some(RemoteNote::NoRemote));
    assert_eq!(by("ssh").remote_note, Some(RemoteNote::Ssh));
    assert_eq!(by("local").remote_note, Some(RemoteNote::NotHttps));
    assert_eq!(by("plain").remote_note, Some(RemoteNote::NotHttps));
    assert_eq!(by("plain").remote_url, None);
    assert_eq!(
        by("broken").unreadable.as_deref(),
        Some("git remote failed: boom")
    );
}

#[test]
fn an_empty_volume_lists_nothing() {
    assert_eq!(parse("D\n").unwrap(), CheckoutList::default());
}

#[test]
fn credentials_never_survive_the_parser() {
    for (raw, shown) in [
        ("https://user:tok@host/x", "https://host/x"),
        ("https://tok@host/x", "https://host/x"),
        ("https://a:b@c@host/x?y=1", "https://host/x?y=1"),
        ("HTTPS://u:p@Host:8443/x", "HTTPS://Host:8443/x"),
    ] {
        let out = format!("C\tr\t{raw}\tmain\tabc\t0\nD\n");
        let found = &parse(&out).unwrap().checkouts[0];
        assert_eq!(found.remote_url.as_deref(), Some(shown), "{raw}");
        assert_eq!(
            found.remote_note,
            Some(RemoteNote::CredentialsRemoved),
            "{raw}"
        );
        assert!(!format!("{found:?}").contains("tok"), "{raw}");
    }
    // An at sign after the host is part of the path, not credentials.
    let out = "C\tr\thttps://host/x@y\tmain\tabc\t0\nD\n";
    let found = &parse(out).unwrap().checkouts[0];
    assert_eq!(found.remote_url.as_deref(), Some("https://host/x@y"));
    assert_eq!(found.remote_note, None);
    // An SSH address with a user name shows nothing at all.
    let out = "C\tr\tssh://git:pw@host/x\tmain\tabc\t0\nD\n";
    let found = &parse(out).unwrap().checkouts[0];
    assert_eq!(
        (found.remote_url.as_deref(), found.remote_note),
        (None, Some(RemoteNote::Ssh))
    );
}

#[test]
fn malformed_or_unfinished_output_is_refused_whole() {
    for (out, want) in [
        ("", "no end marker"),
        ("C\ta\t\t\t\t0\n", "no end marker"),
        ("X\ty\nD\n", "unexpected output line"),
        ("C\ta\t\t\t\t2\nD\n", "unexpected output line"),
        ("C\ta\t\t\t0\nD\n", "unexpected output line"),
        ("C\ta\tb\tc\td\t0\textra\nD\n", "unexpected output line"),
        ("C\t\t\t\t\t0\nD\n", "unexpected output line"),
        ("M\tmany\nD\n", "bad count line"),
        ("M\nD\n", "unexpected output line"),
        ("E\ta\nD\n", "unexpected output line"),
        ("D\nC\ta\t\t\t\t0\n", "after the end marker"),
        ("D\tx\n", "unexpected output line"),
        // A forged record kind that only differs in case.
        ("c\ta\t\t\t\t0\nD\n", "unexpected output line"),
    ] {
        let err = parse(out).unwrap_err();
        assert!(err.contains(want), "{out:?}: {err}");
    }
}

#[test]
fn hostile_output_stays_bounded_and_printable() {
    let long = "x".repeat(5000);
    let out = format!("C\td\u{7}ir\thttps://h/{long}\tbra\u{1b}nch\t{long}\t0\nD\n");
    let found = &parse(&out).unwrap().checkouts[0];
    assert_eq!(found.dir, "d?ir");
    assert_eq!(found.branch.as_deref(), Some("bra?nch"));
    for field in [
        Some(&found.dir),
        found.remote_url.as_ref(),
        found.branch.as_ref(),
        found.head.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(field.chars().count() <= MAX_FIELD, "{field}");
        assert!(!field.chars().any(char::is_control));
    }

    let mut many = String::new();
    for i in 0..=MAX_CHECKOUTS {
        writeln!(many, "C\tr{i}\t\t\t\t0").unwrap();
    }
    assert!(
        parse(&format!("{many}D\n"))
            .unwrap_err()
            .contains("checkouts")
    );

    let mut errors = String::new();
    for i in 0..300 {
        writeln!(errors, "E\tr{i}\t{long}").unwrap();
    }
    let list = parse(&format!("{errors}D\n")).unwrap();
    assert_eq!(list.checkouts.len(), MAX_ERRORS);
    assert!(list.checkouts.iter().all(|c| {
        c.unreadable
            .as_ref()
            .is_some_and(|m| m.chars().count() <= MAX_FIELD)
    }));
    let out = "M\t18446744073709551615\nM\t5\nD\n";
    assert_eq!(parse(out).unwrap().more, u64::MAX, "saturates");
}

proptest::proptest! {
    /// Whatever the guest prints, the answer is a listing or one error, and no field it
    /// keeps is longer than the limit or holds a control character.
    #[test]
    fn any_output_is_a_bounded_listing_or_one_error(text in "(?s).{0,3000}") {
        check_bounded(&text);
    }

    /// The same, for text that is shaped like records, so the parser's branches run.
    #[test]
    fn record_shaped_output_is_a_bounded_listing_or_one_error(
        lines in proptest::collection::vec(
            (
                proptest::sample::select(vec!["C", "M", "E", "D", "X"]),
                proptest::collection::vec("(?s).{0,700}", 0..7),
            ),
            0..12,
        ),
    ) {
        let mut text = String::new();
        for (kind, fields) in &lines {
            writeln!(text, "{kind}\t{}", fields.join("\t")).unwrap();
        }
        check_bounded(&text);
    }
}

fn check_bounded(text: &str) {
    let Ok(list) = parse(text) else { return };
    assert!(list.checkouts.len() <= MAX_CHECKOUTS.max(MAX_ERRORS) * 2);
    for c in &list.checkouts {
        for field in [
            Some(&c.dir),
            c.remote_url.as_ref(),
            c.branch.as_ref(),
            c.head.as_ref(),
            c.unreadable.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(field.chars().count() <= MAX_FIELD);
            assert!(!field.chars().any(char::is_control));
        }
    }
}

#[test]
fn an_address_without_a_scheme_has_no_credentials_to_remove() {
    assert_eq!(without_credentials("host/x"), ("host/x".to_owned(), false));
    assert_eq!(
        without_credentials("https://u:p@host/x"),
        ("https://host/x".to_owned(), true)
    );
}

#[test]
fn a_host_and_path_with_a_colon_is_scp_like_whether_or_not_a_slash_follows() {
    assert!(is_scp_like("git@host:repo"));
    assert!(is_scp_like("host:o/r"));
    assert!(!is_scp_like("c:repo"), "a drive letter is not a host");
    assert!(!is_scp_like("plain"));
}

#[test]
fn long_valid_records_are_cut_and_made_printable() {
    let long = "x\u{7}".repeat(MAX_FIELD);
    let text =
        format!("C\t{long}\thttps://h/{long}\t{long}\t{long}\t1\nE\t{long}\t{long}\nM\t7\nD\n");
    let list = parse(&text).unwrap();
    assert_eq!(list.checkouts.len(), 2);
    assert_eq!(list.more, 7);
    check_bounded(&text);
}

#[test]
fn the_request_runs_the_script_as_root_on_the_mount_point() {
    let l = Layout::new(&WorkspaceId::new("a").unwrap()).unwrap();
    let r = request(&l, None);
    assert_eq!(r.program, "sh");
    assert_eq!(
        r.args,
        ["-c", CHECKOUTS_SH, "puddle-checkouts", "/workspaces/a"]
    );
    assert_eq!(r.user.as_deref(), Some("root"));
    let one = request(&l, Some(&CheckoutDir::new("src").unwrap()));
    assert_eq!(one.args.last().map(String::as_str), Some("src"));
    assert_eq!(one.args.len(), 5);
}
