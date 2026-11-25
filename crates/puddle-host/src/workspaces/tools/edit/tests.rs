// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[test]
fn a_listing_groups_the_tools_by_file_and_marks_the_root() {
    let out = format!(
        "hash\t{HASH}\ntool\tmise.toml\tnode\t22\t1\ntool\tmise.toml\tnpm:prettier\tlatest\t0\ntool\tapi/.tool-versions\tpython\t3.12\t0\nend\n\n"
    );
    let (hash, files) = parse_listing(&out).unwrap();
    assert_eq!(hash.as_str(), HASH);
    assert_eq!(files.len(), 2);
    assert!(files[0].root && files[0].path == "mise.toml");
    assert_eq!(files[0].tools.len(), 2);
    assert!(files[0].tools[0].installed && !files[0].tools[1].installed);
    assert!(!files[1].root && files[1].tools[0].name.as_str() == "python");
    assert_eq!(
        parse_listing("hash\tabsent\nend\n").unwrap().1,
        Vec::<ToolsFile>::new()
    );
}

#[test]
fn a_listing_that_is_not_exactly_the_agents_is_refused_whole() {
    let tool = |fields: &str| format!("hash\t{HASH}\ntool\t{fields}\nend\n");
    let many = format!(
        "hash\tabsent\n{}end\n",
        "tool\tmise.toml\tnode\t22\t1\n".repeat(MAX_RECORDS)
    );
    let long = tool(&format!(
        "mise.toml\tnode\t{}\t1",
        "9".repeat(MAX_FIELD + 1)
    ));
    for bad in [
        String::new(),
        "end\n".to_owned(),
        format!("hash\t{HASH}\n"),
        format!("hash\t{HASH}\nend\ntool\tmise.toml\tnode\t22\t1\n"),
        "hash\tnope\nend\n".to_owned(),
        format!("hash\t{HASH}\tx\nend\n"),
        format!("version\t{HASH}\nend\n"),
        tool("mise.toml\tnode\t22"),
        tool("mise.toml\tnode\t22\t2"),
        tool("\tnode\t22\t1"),
        tool("mise.toml\tbad name\t22\t1"),
        tool("mise.toml\tnode\t-x\t1"),
        tool("mise.toml\tnode\t22\u{1b}\t1"),
        long,
        many,
    ] {
        assert!(parse_listing(&bad).is_none(), "{bad:?}");
    }
}

#[test]
fn the_other_answers_have_one_exact_shape_each() {
    assert_eq!(
        parse_hash(&format!("hash\t{HASH}\nend\n"))
            .unwrap()
            .as_str(),
        HASH
    );
    for bad in [
        "end\n",
        "hash\tabsent\nhash\tabsent\nend\n",
        "tool\tx\nend\n",
        "hash\tabsent\n",
    ] {
        assert!(parse_hash(bad).is_none(), "{bad:?}");
    }
    let registry = parse_registry("reg\tnode\tJS runtime\nreg\tgo\t\nend\n").unwrap();
    assert_eq!(registry[0].description, "JS runtime");
    assert_eq!(registry[1].description, "");
    for bad in [
        "reg\tnode\nend\n",
        "reg\tbad name\tx\nend\n",
        "x\ty\tz\nend\n",
    ] {
        assert!(parse_registry(bad).is_none(), "{bad:?}");
    }
    let versions = parse_versions("ver\t22.1.0\nver\t22.0.0\nend\n").unwrap();
    assert_eq!(versions.len(), 2);
    for bad in ["ver\t-x\nend\n", "ver\na\tb\nend\n", "version\t1\nend\n"] {
        assert!(parse_versions(bad).is_none(), "{bad:?}");
    }
    let too_many = format!("{}end\n", "ver\t1\n".repeat(201));
    assert!(parse_versions(&too_many).is_none());
}

#[test]
fn a_call_reads_its_answer_names_a_changed_file_and_quotes_mises_last_line() {
    let said = |code, out: &str, err: &str| read(Ok(ExecOutput::new(code, out, err)), parse_hash);
    assert!(said(0, "hash\tabsent\nend\n", "").is_ok());
    assert_eq!(
        said(5, "", "mise.toml changed since it was listed"),
        Err(Failed::Changed)
    );
    assert_eq!(
        said(
            1,
            "",
            "mise WARN slow\nmise ERROR no route to mise-versions.jdx.dev\n\n"
        ),
        Err(Failed::Reason(
            "mise ERROR no route to mise-versions.jdx.dev".to_owned()
        ))
    );
    assert_eq!(
        said(137, "", " \n"),
        Err(Failed::Reason("mise ended with exit code 137".to_owned()))
    );
    assert_eq!(
        said(0, "garbage\n", ""),
        Err(Failed::Reason(
            "the guest's answer could not be read".to_owned()
        ))
    );
    let gone = ComputeError::NotFound {
        sandbox: "acme".to_owned(),
    };
    assert_eq!(
        read(Err(gone), parse_hash),
        Err(Failed::Reason("sandbox \"acme\" not found".to_owned()))
    );
    assert_eq!(
        reason_of(Failed::Changed),
        "the guest's answer could not be read"
    );
}

#[test]
fn a_view_without_a_listing_has_no_time_hash_or_files() {
    let none = view(false, None, Some("why".to_owned()));
    assert_eq!(
        (
            none.listed_at,
            none.sha256,
            none.files.len(),
            none.problem.as_deref()
        ),
        (None, None, 0, Some("why"))
    );
    let listed = Listed {
        at: 7,
        hash: ToolsFileHash::absent(),
        files: Vec::new(),
    };
    let shown = view(true, Some(&listed), None);
    assert_eq!((shown.listed_at, shown.running), (Some(7), true));
    assert_eq!(shown.sha256, Some(ToolsFileHash::absent()));
}

#[test]
fn a_failed_mise_reason_has_no_control_or_bidi_characters_and_is_cut() {
    let Err(Failed::Reason(said)) = read(
        Ok(ExecOutput::new(1, "", "\u{1b}[31mmise ERROR\u{202e}\n")),
        parse_hash,
    ) else {
        panic!("a reason");
    };
    assert_eq!(said, "?[31mmise ERROR?");
    let Err(Failed::Reason(said)) = read(
        Ok(ExecOutput::new(1, "", "z".repeat(1_000_000))),
        parse_hash,
    ) else {
        panic!("a reason");
    };
    assert_eq!(said.chars().count(), MAX_FIELD);
}

#[test]
fn a_cut_listing_is_refused_even_when_what_is_left_parses() {
    let mut out = ExecOutput::new(0, format!("hash\t{HASH}\nend\n"), "");
    assert!(read(Ok(out.clone()), parse_listing).is_ok());
    out.stdout_truncated = true;
    let Err(Failed::Reason(why)) = read(Ok(out), parse_listing) else {
        panic!("a cut listing was accepted");
    };
    assert!(why.contains("cut"), "{why}");
}
