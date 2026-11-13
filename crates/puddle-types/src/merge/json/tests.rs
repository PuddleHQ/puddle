// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;
use serde_json::json;

use super::super::{MergeFormat, MergeSpec, Merged, unmerge as unmerge_bytes};
use super::*;

fn k(path: &str) -> Vec<String> {
    path.split('.').map(str::to_owned).collect()
}

fn docker_spec() -> MergeSpec {
    MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(
            &["proxies", "default"],
            &json!({"httpProxy": "http://172.17.0.1:3128", "httpsProxy": "http://172.17.0.1:3128"}),
        )],
    )
    .unwrap()
}

fn applied(spec: &MergeSpec, file: &str, previous: &[Vec<String>]) -> String {
    match spec.apply(Some(file.as_bytes()), previous).unwrap() {
        Merged::Write(b) => String::from_utf8(b).unwrap(),
        Merged::Unchanged => file.to_owned(),
    }
}

/// What `docker login` leaves behind: auths and helpers, tab-indented.
const DOCKER_LOGIN: &str = "{\n\t\"auths\": {\n\t\t\"registry.example.test\": {\n\t\t\t\"auth\": \"dXNlcjpwYXNz\"\n\t\t}\n\t},\n\t\"credHelpers\": {\n\t\t\"gcr.io\": \"gcloud\"\n\t}\n}";

#[test]
fn docker_login_entries_survive_and_proxies_are_added_in_the_files_layout() {
    let spec = docker_spec();
    let out = applied(&spec, DOCKER_LOGIN, &[]);
    assert_eq!(
        out,
        "{\n\t\"auths\": {\n\t\t\"registry.example.test\": {\n\t\t\t\"auth\": \"dXNlcjpwYXNz\"\n\t\t}\n\t},\n\t\"credHelpers\": {\n\t\t\"gcr.io\": \"gcloud\"\n\t},\n\t\"proxies\": {\n\t\t\"default\": {\n\t\t\t\"httpProxy\": \"http://172.17.0.1:3128\",\n\t\t\t\"httpsProxy\": \"http://172.17.0.1:3128\"\n\t\t}\n\t}\n}"
    );
    // The second boot leaves it alone, byte for byte.
    assert_eq!(
        spec.apply(Some(out.as_bytes()), &spec.keys()).unwrap(),
        Merged::Unchanged
    );
}

#[test]
fn a_changed_value_is_rewritten_in_place_and_nothing_else_moves() {
    let file = "{ \"auths\" :{\"r\":{ \"auth\":\"x\"}} ,\n  \"proxies\": {\n    \"default\": {\"httpProxy\": \"http://old\"},\n    \"tcp://other:2376\": {\"httpProxy\": \"http://user\"}\n  },\"z\":1.50}";
    let out = applied(&docker_spec(), file, &[]);
    assert_eq!(
        out,
        "{ \"auths\" :{\"r\":{ \"auth\":\"x\"}} ,\n  \"proxies\": {\n    \"default\": {\n      \"httpProxy\": \"http://172.17.0.1:3128\",\n      \"httpsProxy\": \"http://172.17.0.1:3128\"\n    },\n    \"tcp://other:2376\": {\"httpProxy\": \"http://user\"}\n  },\"z\":1.50}"
    );
}

#[test]
fn minified_files_get_compact_members() {
    let spec = docker_spec();
    let out = applied(&spec, r#"{"auths":{}}"#, &[]);
    assert_eq!(
        out,
        r#"{"auths":{}, "proxies": {"default":{"httpProxy":"http://172.17.0.1:3128","httpsProxy":"http://172.17.0.1:3128"}}}"#
    );
    let out = applied(&spec, r#"{"proxies":{"x":1}}"#, &[]);
    assert_eq!(
        out,
        r#"{"proxies":{"x":1, "default": {"httpProxy":"http://172.17.0.1:3128","httpsProxy":"http://172.17.0.1:3128"}}}"#
    );
}

#[test]
fn an_empty_object_takes_the_files_indent() {
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["a", "b"], &json!(1))],
    )
    .unwrap();
    assert_eq!(
        applied(&spec, "{\n  \"x\": 1,\n  \"a\": {}\n}\n", &[]),
        "{\n  \"x\": 1,\n  \"a\": {\n    \"b\": 1\n  }\n}\n"
    );
    assert_eq!(
        applied(&spec, "{}", &[]),
        "{\n\t\"a\": {\n\t\t\"b\": 1\n\t}\n}"
    );
}

#[test]
fn keys_puddle_no_longer_owns_are_removed_with_their_empty_parents() {
    let old = MergeSpec::new(
        MergeFormat::Json,
        vec![
            MergeEntry::json(&["proxies", "default"], &json!({"httpProxy": "x"})),
            MergeEntry::json(&["features", "puddle"], &json!(true)),
        ],
    )
    .unwrap();
    let with_user = applied(&old, DOCKER_LOGIN, &[]);
    let out = applied(&docker_spec(), &with_user, &old.keys());
    assert!(!out.contains("features"), "{out}");
    assert!(
        out.contains("httpsProxy") && out.contains("dXNlcjpwYXNz"),
        "{out}"
    );
    // Removing everything Puddle owns gives back the user's file exactly.
    let u = unmerge_bytes(MergeFormat::Json, out.as_bytes(), &docker_spec().keys()).unwrap();
    assert_eq!(u.merged, Merged::Write(DOCKER_LOGIN.as_bytes().to_vec()));
    assert!(!u.empty);
}

#[test]
fn a_parent_the_user_also_uses_is_kept() {
    let file = r#"{"proxies": {"default": {"httpProxy": "x"}, "tcp://h": {}}}"#;
    let (out, empty) = unmerge(Dialect::Strict, file, &[&k("proxies.default")]).unwrap();
    assert_eq!(out, r#"{"proxies": {"tcp://h": {}}}"#);
    assert!(!empty);
    let (out, empty) = unmerge(
        Dialect::Strict,
        "{\n\t\"proxies\": {\n\t\t\"default\": 1\n\t}\n}\n",
        &[&k("proxies.default")],
    )
    .unwrap();
    assert_eq!(out, "{}\n");
    assert!(empty);
}

#[test]
fn removal_handles_first_middle_last_and_missing_members() {
    let file = "{\n  \"a\": 1,\n  \"b\": [1, {\"c\": \"}\"}],\n  \"d\": \"\\\"\"\n}";
    assert_eq!(
        unmerge(Dialect::Strict, file, &[&k("a")]).unwrap().0,
        "{\n  \"b\": [1, {\"c\": \"}\"}],\n  \"d\": \"\\\"\"\n}"
    );
    assert_eq!(
        unmerge(Dialect::Strict, file, &[&k("b")]).unwrap().0,
        "{\n  \"a\": 1,\n  \"d\": \"\\\"\"\n}"
    );
    assert_eq!(
        unmerge(Dialect::Strict, file, &[&k("d")]).unwrap().0,
        "{\n  \"a\": 1,\n  \"b\": [1, {\"c\": \"}\"}]\n}"
    );
    assert_eq!(
        unmerge(Dialect::Strict, file, &[&k("x"), &k("a.b"), &k("b.c")])
            .unwrap()
            .0,
        file
    );
}

#[test]
fn non_object_parents_and_duplicate_keys_are_refused() {
    let spec = docker_spec();
    assert_eq!(
        spec.apply(Some(br#"{"proxies": "nope"}"#), &[]),
        Err(MergeRefusal::Conflict {
            key: "proxies".into()
        })
    );
    assert_eq!(
        spec.apply(Some(br#"{"proxies": {}, "proxies": {}}"#), &[]),
        Err(MergeRefusal::DuplicateKey {
            key: "proxies".into()
        })
    );
    // A duplicate elsewhere is the user's business and stays as it is.
    let out = applied(&spec, r#"{"a": 1, "a": 2}"#, &[]);
    assert!(out.starts_with(r#"{"a": 1, "a": 2, "proxies""#), "{out}");
}

#[test]
fn keys_with_escapes_and_unicode_are_matched_by_value() {
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["é\"x"], &json!("v"))],
    )
    .unwrap();
    let out = applied(&spec, r#"{"é\"x": "old", "ü": 1}"#, &[]);
    assert_eq!(out, r#"{"é\"x": "v", "ü": 1}"#);
}

#[test]
fn deep_nesting_is_refused_by_the_parser_not_a_stack_overflow() {
    let deep = format!("{{\"a\": {}{}}}", "[".repeat(100_000), "]".repeat(100_000));
    assert!(matches!(
        docker_spec().apply(Some(deep.as_bytes()), &[]),
        Err(MergeRefusal::Syntax { .. })
    ));
}

// --- JSONC -------------------------------------------------------------------------------

fn vscode_spec() -> MergeSpec {
    MergeSpec::new(
        MergeFormat::Jsonc,
        vec![
            MergeEntry::json(&["remote.autoForwardPortsSource"], &json!("process")),
            MergeEntry::json(
                &["remote.portsAttributes", "3128"],
                &json!({"onAutoForward": "ignore"}),
            ),
        ],
    )
    .unwrap()
}

fn one(key: &str, value: &Value) -> MergeSpec {
    MergeSpec::new(MergeFormat::Jsonc, vec![MergeEntry::json(&[key], value)]).unwrap()
}

fn unmerged(spec: &MergeSpec, file: &str) -> String {
    let u = unmerge_bytes(spec.format(), file.as_bytes(), &spec.keys()).unwrap();
    match u.merged {
        Merged::Write(b) => String::from_utf8(b).unwrap(),
        Merged::Unchanged => file.to_owned(),
    }
}

/// A settings file as a person writes it: comments, trailing commas, 4-space indent.
const USER_SETTINGS: &str = "// My settings\n{\n    // the font\n    \"editor.fontSize\": 14, // big\n    /* theme */\n    \"workbench.colorTheme\": \"Monokai\",\n    \"remote.portsAttributes\": {\n        \"8080\": { \"label\": \"web\" }, // mine\n    },\n}\n";

#[test]
fn the_users_comments_and_commas_survive_a_merge_and_puddles_keys_arrive() {
    let spec = vscode_spec();
    let out = applied(&spec, USER_SETTINGS, &[]);
    assert_eq!(
        out,
        "// My settings\n{\n    // the font\n    \"editor.fontSize\": 14, // big\n    /* theme */\n    \"workbench.colorTheme\": \"Monokai\",\n    \"remote.portsAttributes\": {\n        \"8080\": { \"label\": \"web\" }, // mine\n        \"3128\": {\n            \"onAutoForward\": \"ignore\"\n        }\n    },\n    \"remote.autoForwardPortsSource\": \"process\"\n}\n"
    );
}

#[test]
fn a_second_apply_changes_nothing_and_unmerge_keeps_the_users_comments() {
    let spec = vscode_spec();
    let out = applied(&spec, USER_SETTINGS, &[]);
    assert_eq!(
        spec.apply(Some(out.as_bytes()), &spec.keys()).unwrap(),
        Merged::Unchanged
    );
    let back = unmerged(&spec, &out);
    for kept in [
        "// My settings",
        "// the font",
        "// big",
        "/* theme */",
        "// mine",
    ] {
        assert!(back.contains(kept), "{kept} lost: {back}");
    }
    assert!(
        !back.contains("3128") && !back.contains("process"),
        "{back}"
    );
    assert!(blank(&back).is_ok());
}

#[test]
fn a_trailing_comment_stays_on_the_last_users_line() {
    let spec = one("z", &json!(1));
    let file = "{\n  \"a\": 1 // keep me\n}\n";
    let out = applied(&spec, file, &[]);
    assert_eq!(out, "{\n  \"a\": 1, // keep me\n  \"z\": 1\n}\n");
    assert_eq!(unmerged(&spec, &out), file);
    // With a trailing comma already there.
    let out = applied(&spec, "{\n  \"a\": 1, // keep me\n}\n", &[]);
    assert_eq!(out, "{\n  \"a\": 1, // keep me\n  \"z\": 1\n}\n");
}

#[test]
fn a_changed_value_is_replaced_and_the_comments_around_it_stay() {
    let spec = one("a", &json!("new"));
    let out = applied(
        &spec,
        "{\n  // why\n  \"a\": \"old\", // note\n  \"b\": 1,\n}",
        &[],
    );
    assert_eq!(
        out,
        "{\n  // why\n  \"a\": \"new\", // note\n  \"b\": 1,\n}"
    );
}

#[test]
fn dropped_keys_go_and_the_comment_above_the_next_key_stays() {
    let old = MergeSpec::new(
        MergeFormat::Jsonc,
        vec![
            MergeEntry::json(&["gone"], &json!(1)),
            MergeEntry::json(&["kept"], &json!(2)),
        ],
    )
    .unwrap();
    let new = one("kept", &json!(2));
    let file = "{\n  \"gone\": 1,\n  // about b\n  \"b\": 3,\n  \"kept\": 2, // mine\n}";
    assert_eq!(
        applied(&new, file, &old.keys()),
        "{\n  // about b\n  \"b\": 3,\n  \"kept\": 2, // mine\n}"
    );
    // A later member: the comment on the line before it stays.
    let file = "{\n  \"b\": 3, // three\n  \"gone\": 1\n}";
    let out = applied(&new, file, &old.keys());
    assert!(out.contains("// three") && !out.contains("gone"), "{out}");
    assert!(blank(&out).is_ok());
}

#[test]
fn a_parent_with_a_comment_stays_when_puddles_key_leaves() {
    let spec = vscode_spec();
    let file = "{\n  \"remote.portsAttributes\": {\n    // mine\n    \"3128\": { \"onAutoForward\": \"ignore\" }\n  }\n}\n";
    let u = unmerge_bytes(MergeFormat::Jsonc, file.as_bytes(), &spec.keys()).unwrap();
    let Merged::Write(b) = u.merged else { panic!() };
    let out = String::from_utf8(b).unwrap();
    assert!(
        out.contains("// mine") && out.contains("remote.portsAttributes"),
        "{out}"
    );
    assert!(!u.empty);
}

#[test]
fn a_file_with_only_comments_counts_as_empty_and_keeps_them() {
    let spec = vscode_spec();
    let out = applied(&spec, "// nothing yet\n", &[]);
    assert!(
        out.starts_with("// nothing yet\n{\n\t\"remote.autoForwardPortsSource\""),
        "{out}"
    );
    let u = unmerge_bytes(MergeFormat::Jsonc, b"// nothing yet\n", &spec.keys()).unwrap();
    assert_eq!(u.merged, Merged::Unchanged);
    assert!(!u.empty, "the user's comment is still there");
    assert!(unmerged(&spec, &out).contains("// nothing yet"));
}

#[test]
fn a_comment_only_object_gets_the_member_in_front_of_it() {
    let out = applied(&one("a", &json!(1)), "{\n  // empty\n}\n", &[]);
    assert_eq!(out, "{\n\t\"a\": 1\n  // empty\n}\n");
}

#[test]
fn a_file_puddle_wrote_is_empty_again_after_removal() {
    let spec = vscode_spec();
    let u = unmerge_bytes(MergeFormat::Jsonc, &spec.fresh(), &spec.keys()).unwrap();
    assert!(u.empty);
}

#[test]
fn invalid_jsonc_is_refused_and_never_quoted() {
    let spec = vscode_spec();
    for bad in [
        "{\"secret\": 1 /* open",
        "{\"secret\": }",
        "{\"a\": 1,, }",
        "[1] // not an object",
        "{\"a\": 1} {\"b\": 2}",
    ] {
        let e = spec.apply(Some(bad.as_bytes()), &[]).unwrap_err();
        assert!(!e.to_string().contains("secret"), "{e}");
        assert!(
            matches!(
                e,
                MergeRefusal::Syntax {
                    format: "JSONC",
                    ..
                } | MergeRefusal::NotAnObject
            ),
            "{bad}: {e:?}"
        );
        assert!(unmerge_bytes(MergeFormat::Jsonc, bad.as_bytes(), &spec.keys()).is_err());
    }
}

#[test]
fn a_non_object_on_an_owned_path_and_duplicates_are_refused_in_jsonc_too() {
    let spec = vscode_spec();
    assert_eq!(
        spec.apply(Some(b"{ // c\n \"remote.portsAttributes\": 3 }"), &[]),
        Err(MergeRefusal::Conflict {
            key: "remote.portsAttributes".into()
        })
    );
    assert_eq!(
            spec.apply(
                Some(b"{\"remote.autoForwardPortsSource\": 1, /* x */ \"remote.autoForwardPortsSource\": 2}"),
                &[]
            ),
            Err(MergeRefusal::DuplicateKey {
                key: "remote.autoForwardPortsSource".into()
            })
        );
}

#[test]
fn comment_syntax_is_not_accepted_in_strict_json() {
    assert!(matches!(
        docker_spec().apply(Some(b"{ // c\n}"), &[]),
        Err(MergeRefusal::Syntax { format: "JSON", .. })
    ));
}

#[test]
fn crlf_files_keep_their_line_breaks() {
    let out = applied(&one("z", &json!(1)), "{\r\n  \"a\": 1 // c\r\n}\r\n", &[]);
    assert!(
        out.starts_with("{\r\n  \"a\": 1, // c\n  \"z\": 1"),
        "{out:?}"
    );
    assert!(out.ends_with("\r\n}\r\n"));
}

/// Every line of a pretty document gets a trailing comment; a comment opens the file.
fn commented(text: &str) -> String {
    let mut out = String::from("/* head */\n");
    for line in text.lines() {
        out.push_str(line);
        out.push_str(" // c\n");
    }
    out
}

proptest! {
    #[test]
    fn jsonc_merges_keep_every_comment_and_converge(
        (doc, _) in arb_doc(),
        value in arb_json(),
    ) {
        let text = commented(&serde_json::to_string_pretty(&doc).unwrap());
        let spec = MergeSpec::new(MergeFormat::Jsonc, vec![MergeEntry {
            key: vec!["zz".into()],
            value: value.to_string(),
        }]).unwrap();
        let out = match spec.apply(Some(text.as_bytes()), &[]).unwrap() {
            Merged::Write(b) => String::from_utf8(b).unwrap(),
            Merged::Unchanged => text.clone(),
        };
        prop_assert!(out.starts_with("/* head */"));
        prop_assert!(out.matches("// c").count() >= text.matches("// c").count());
        prop_assert_eq!(
            spec.apply(Some(out.as_bytes()), &spec.keys()).unwrap(),
            Merged::Unchanged
        );
        // Removing the key gives the same document, comments included.
        let back = unmerged(&spec, &out);
        prop_assert_eq!(back.matches("// c").count(), text.matches("// c").count());
        let back: Value = serde_json::from_str(&blank(&back).unwrap().view).unwrap();
        prop_assert_eq!(back, doc);
    }

    #[test]
    fn arbitrary_jsonc_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = vscode_spec().apply(Some(&bytes), &[k("a.b")]);
        let _ = unmerge_bytes(MergeFormat::Jsonc, &bytes, &[k("a")]);
    }
}

fn arb_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i32>().prop_map(|n| json!(n)),
        "[a-z\"\\\\é ]{0,6}".prop_map(Value::String),
    ];
    leaf.prop_recursive(4, 32, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("[a-c]{1,2}", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn arb_doc() -> impl Strategy<Value = (Value, String)> {
    (
        prop::collection::btree_map("[a-c]{1,2}", arb_json(), 0..5),
        0..3usize,
    )
        .prop_map(|(m, style)| {
            let v = Value::Object(m.into_iter().collect());
            let text = match style {
                0 => v.to_string(),
                1 => serde_json::to_string_pretty(&v).unwrap(),
                _ => pretty(&v, "\t", "") + "\n",
            };
            (v, text)
        })
}

fn arb_key() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-c]{1,2}", 1..3)
}

/// `v` without `key` and without the objects on its path that are (then) empty.
fn strip(mut v: Value, key: &[String]) -> Value {
    value_delete(&mut v, key);
    for len in (1..key.len()).rev() {
        let path = &key[..len];
        let empty = path
            .iter()
            .try_fold(&v, |cur, k| cur.get(k))
            .is_some_and(|o| o.as_object().is_some_and(Map::is_empty));
        if empty {
            let (last, parents) = path.split_last().unwrap();
            let parent = parents
                .iter()
                .fold(&mut v, |cur, k| cur.get_mut(k).unwrap());
            parent.as_object_mut().unwrap().remove(last);
        }
    }
    v
}

proptest! {
    #[test]
    fn merge_keeps_the_rest_and_converges(
        (doc, text) in arb_doc(),
        key in arb_key(),
        value in arb_json(),
    ) {
        let spec = MergeSpec::new(MergeFormat::Json, vec![MergeEntry {
            key: key.clone(),
            value: value.to_string(),
        }]).unwrap();
        match spec.apply(Some(text.as_bytes()), &[]) {
            Ok(merged) => {
                let out = match merged {
                    Merged::Write(b) => String::from_utf8(b).unwrap(),
                    Merged::Unchanged => text.clone(),
                };
                // The self-check passed, so the result is the value-level merge; applying
                // again changes nothing, byte for byte.
                prop_assert_eq!(spec.apply(Some(out.as_bytes()), &spec.keys()).unwrap(), Merged::Unchanged);
                // Removing the key again gives the original minus that key.
                let back = unmerge_bytes(MergeFormat::Json, out.as_bytes(), &spec.keys()).unwrap();
                let back = match back.merged {
                    Merged::Write(b) => String::from_utf8(b).unwrap(),
                    Merged::Unchanged => out.clone(),
                };
                // (Up to empty objects on the key's path, which the removal prunes.)
                prop_assert_eq!(
                    strip(serde_json::from_str::<Value>(&back).unwrap(), &key),
                    strip(doc.clone(), &key)
                );
            }
            // A non-object on the path is the only reason to refuse a valid document.
            Err(e) => prop_assert!(matches!(e, MergeRefusal::Conflict { .. }), "{e}"),
        }
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = docker_spec().apply(Some(&bytes), &[k("a.b")]);
        let _ = unmerge_bytes(MergeFormat::Json, &bytes, &[k("a")]);
    }
}

#[test]
fn a_comment_only_file_without_a_final_newline_gets_the_object_on_its_own_line() {
    let spec = vscode_spec();
    let out = applied(&spec, "// mine", &[]);
    assert!(out.starts_with("// mine\n{"), "{out}");
}

const REFUSED: Result<usize, MergeRefusal> = Err(MergeRefusal::Inconsistent);

#[test]
fn the_scanner_refuses_text_that_is_not_the_valid_json_it_expects() {
    assert_eq!(skip_string(b"\"open", 0), REFUSED);
    assert_eq!(skip_value(b"", 0), REFUSED);
    assert_eq!(skip_value(b"{\"a\": [", 0), REFUSED);
    assert_eq!(skip_value(b"\"open", 0), REFUSED);
    assert_eq!(skip_value(b"[\"open", 0), REFUSED);
    assert_eq!(skip_value(b"nul", 0), Ok(3));
    assert_eq!(skip_value(b"[1, \"]\"]x", 0), Ok(8));
}

fn refused_object(text: &str) -> bool {
    matches!(object_at(text, 0), Err(MergeRefusal::Inconsistent))
}

#[test]
fn an_object_scan_refuses_what_is_not_an_object_with_string_keys() {
    assert!(refused_object("[1]"));
    assert!(refused_object("{1: 2}"));
    assert!(refused_object("{\"a\" 1}"));
    assert!(refused_object("{\"a\":"));
    assert!(refused_object("{\"\\q\": 1}"));
    assert!(!refused_object("{\"a\": 1, \"b\": {\"c\": [1]}}"));
}

#[test]
fn edits_to_an_empty_key_path_are_refused() {
    let value = json!(1);
    assert!(matches!(
        set(Dialect::Strict, "{}", &[], &value),
        Err(MergeRefusal::Inconsistent)
    ));
    assert!(matches!(find("{}", &[]), Err(MergeRefusal::Inconsistent)));
    assert!(matches!(
        remove(Dialect::Strict, "{}", &[]),
        Ok((t, None)) if t == "{}"
    ));
}

#[test]
fn a_splice_outside_the_text_is_refused() {
    assert_eq!(splice("abc", 1, 2, "X").unwrap(), "aXc");
    assert_eq!(splice("abc", 9, 9, "X"), Err(MergeRefusal::Inconsistent));
    assert_eq!(splice("abc", 1, 9, "X"), Err(MergeRefusal::Inconsistent));
}

#[test]
fn the_self_check_refuses_a_text_that_differs_from_the_expected_value() {
    let expected = json!({"a": 1});
    assert_eq!(check(Dialect::Strict, "{\"a\": 1}", &expected), Ok(()));
    assert_eq!(
        check(Dialect::Strict, "{\"a\": 2}", &expected),
        Err(MergeRefusal::Inconsistent)
    );
    assert_eq!(
        check(Dialect::Strict, "{", &expected),
        Err(MergeRefusal::Inconsistent)
    );
}

#[test]
fn value_edits_on_an_empty_path_or_through_a_non_object_change_nothing() {
    let mut v = json!({"a": 1});
    value_set(&mut v, &[], json!(2));
    value_set(&mut v, &k("a.b"), json!(2));
    value_delete(&mut v, &[]);
    value_delete(&mut v, &k("a.b"));
    value_delete(&mut v, &k("x.y"));
    assert_eq!(v, json!({"a": 1}));
    value_set(&mut v, &k("x.y"), json!(3));
    assert_eq!(v, json!({"a": 1, "x": {"y": 3}}));
}

#[test]
fn removing_a_key_through_a_duplicated_parent_is_refused_with_the_duplicate_named() {
    let err = remove(Dialect::Strict, r#"{"a": {}, "a": {}}"#, &k("a.b")).unwrap_err();
    assert_eq!(err, MergeRefusal::DuplicateKey { key: "a".into() });
}

#[test]
fn applying_the_same_spec_twice_leaves_the_second_file_as_it_is() {
    let spec = docker_spec();
    let once = applied(&spec, DOCKER_LOGIN, &[]);
    assert_eq!(applied(&spec, &once, &spec.keys()), once);
}

#[test]
fn unmerging_a_file_without_the_keys_leaves_it_as_it_is() {
    let file = "{\"auths\": {}}";
    assert_eq!(unmerged(&docker_spec(), file), file);
}

#[test]
fn a_member_that_is_the_last_of_several_is_removed_with_its_comma() {
    let key = k("b");
    let (out, top) = remove(Dialect::Strict, "{\"a\": 1, \"b\": 2}", &key).unwrap();
    assert_eq!(out, "{\"a\": 1}");
    assert_eq!(top, Some(1));
}
