// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn p(s: &str) -> GuestPath {
    GuestPath::new(s).unwrap()
}

#[test]
fn an_owner_only_file_never_prints_its_contents() {
    let secret = GuestFile::new(p("/etc/puddle/key"), b"CANARY-KEY".to_vec())
        .with_mode(FileMode::new(0o600));
    let shown = format!("{secret:?}");
    let leaked_text = shown.contains("CANARY");
    let leaked_bytes = shown.contains("67, 65");
    assert!(!leaked_text, "the Debug output spells the contents");
    assert!(!leaked_bytes, "the Debug output lists the content bytes");
    assert!(
        shown.contains("10 bytes"),
        "the Debug output omits the size"
    );
    let public = GuestFile::new(p("/etc/puddle/cert"), b"PUBLIC".to_vec());
    assert!(format!("{public:?}").contains("80, 85, 66"));
}

#[test]
fn a_plain_file_replaces_and_a_merged_file_starts_from_the_specs_fresh_text() {
    use crate::{MergeEntry, MergeFormat};
    let plain = GuestFile::new(p("/etc/a"), b"x".to_vec());
    assert!(matches!(plain.apply(), ApplyKind::Replace));
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(&["a"], &serde_json::json!(1))],
    )
    .unwrap();
    let merged = GuestFile::merged(p("/root/.docker/config.json"), spec.clone());
    assert_eq!(merged.path(), &p("/root/.docker/config.json"));
    assert_eq!(merged.contents(), b"{\n\t\"a\": 1\n}\n");
    assert_eq!(merged.mode(), GuestFile::DEFAULT_MODE);
    assert!(matches!(merged.apply(), ApplyKind::Merge(s) if *s == spec));
}

#[test]
fn guest_path_accepts_normalised_absolute_paths() {
    for ok in [
        "/",
        "/a",
        "/a/b.c",
        "/.hidden",
        "/a/..b",
        "/workspaces/acme",
    ] {
        assert_eq!(p(ok).as_str(), ok);
        assert_eq!(p(ok).to_string(), ok);
    }
}

#[test]
fn guest_path_rejects_everything_else() {
    let cases = [
        ("", "absolute"),
        ("a/b", "absolute"),
        ("/a/", "normalised"),
        ("//a", "normalised"),
        ("/a//b", "normalised"),
        ("/a/./b", "normalised"),
        ("/a/../b", "normalised"),
        ("/..", "normalised"),
        ("/a\0b", "NUL"),
    ];
    for (bad, reason) in cases {
        let err = GuestPath::new(bad).unwrap_err();
        assert!(err.reason().contains(reason), "{bad:?}: {err}");
    }
    let long = format!("/{}", "a".repeat(MAX_PATH_LEN));
    assert!(GuestPath::new(&long).unwrap_err().reason().contains("4096"));
}

#[test]
fn within_is_component_wise() {
    assert!(p("/a/b").is_within(&p("/a")));
    assert!(p("/a").is_within(&p("/a")));
    assert!(p("/a").is_within(&p("/")));
    assert!(!p("/ab").is_within(&p("/a")));
    assert!(!p("/a").is_within(&p("/a/b")));
    assert_eq!(p("/a/b/c").relative_to(&p("/a")), Some("b/c"));
    assert_eq!(p("/a").relative_to(&p("/a")), Some(""));
    assert_eq!(p("/a/b").relative_to(&p("/")), Some("a/b"));
    assert_eq!(p("/").relative_to(&p("/")), Some(""));
}

#[test]
fn guest_path_conversions() {
    assert_eq!(GuestPath::try_from("/x").unwrap(), p("/x"));
    assert_eq!(GuestPath::try_from(String::from("/x")).unwrap(), p("/x"));
    assert_eq!(String::from(p("/x")), "/x");
    assert!(serde_json::from_str::<GuestPath>(r#""/a/../b""#).is_err());
    assert_eq!(serde_json::to_string(&p("/x")).unwrap(), r#""/x""#);
}

#[test]
fn guest_file_defaults_and_mode_check() {
    let f = GuestFile::new(p("/etc/x"), b"hi".to_vec());
    assert_eq!(f.mode(), GuestFile::DEFAULT_MODE);
    assert_eq!(f.path(), &p("/etc/x"));
    assert_eq!(f.contents(), b"hi");
    assert_eq!(f.with_mode(FileMode::new(0o4755)).mode(), 0o4755);
    let err = FileMode::try_new(0o10000).unwrap_err();
    assert_eq!(err.what(), "file mode");
}

#[test]
fn the_build_time_path_check_agrees_with_the_runtime_one() {
    let long = format!("/{}", "a".repeat(4096));
    let samples = [
        "/",
        "/a",
        "/a/b",
        "/a/b/",
        "//",
        "/a//b",
        "/.",
        "/..",
        "/a/.",
        "/a/..",
        "/a/./b",
        "/a/../b",
        "/...",
        "/.a",
        "/a.",
        "/a\0b",
        "a",
        "",
        "relative/x",
        "/ab/cd/ef",
        "/a/b/c/",
        &long,
    ];
    for sample in samples {
        assert_eq!(
            is_valid_path(sample),
            GuestPath::new(sample).is_ok(),
            "{sample:?}"
        );
    }
}

#[test]
fn guest_env_names_and_values_are_checked() {
    let mut env = GuestEnv::new();
    assert!(env.is_empty());
    for ok in ["A", "_", "a_1", "NODE_EXTRA_CA_CERTS"] {
        env.set(ok, "v").unwrap();
    }
    for bad in ["", "1A", "A-B", "A B", "Ä"] {
        assert!(env.set(bad, "v").is_err(), "{bad:?}");
    }
    assert!(env.set("A", "nul\0").is_err());
    assert_eq!(env.len(), 4);
}

#[test]
fn guest_env_later_values_win() {
    let mut a = GuestEnv::new();
    a.set("X", "1").unwrap();
    a.set("Y", "1").unwrap();
    let mut b = GuestEnv::new();
    b.set("X", "2").unwrap();
    a.extend(&b);
    assert_eq!(a.get("X"), Some("2"));
    assert_eq!(a.get("Y"), Some("1"));
    assert_eq!(a.get("Z"), None);
    let pairs: Vec<_> = a.iter().collect();
    assert_eq!(pairs, [("X", "2"), ("Y", "1")]);
}

#[test]
fn guest_env_serde_validates() {
    let env: GuestEnv = serde_json::from_str(r#"{"A":"1"}"#).unwrap();
    assert_eq!(env.get("A"), Some("1"));
    assert_eq!(serde_json::to_string(&env).unwrap(), r#"{"A":"1"}"#);
    assert!(serde_json::from_str::<GuestEnv>(r#"{"A-B":"1"}"#).is_err());
}

#[test]
fn a_guest_path_literal_is_the_same_path_as_a_parsed_one() {
    let from_lit = GuestPath::from(GuestPathLit::new("/etc/puddle/key"));
    assert_eq!(from_lit, p("/etc/puddle/key"));
    assert_eq!(guest_path!("/"), p("/"));
}

#[test]
#[should_panic(expected = "not a valid guest path")]
fn an_invalid_guest_path_literal_panics_which_stops_a_const_build() {
    let _ = GuestPathLit::new("relative");
}

#[test]
fn a_file_mode_within_the_permission_bits_is_accepted() {
    assert_eq!(FileMode::try_new(0o4755).unwrap().bits(), 0o4755);
    assert_eq!(FileMode::try_new(0o7777).unwrap(), FileMode::new(0o7777));
}

#[test]
#[should_panic(expected = "a file mode is within 0o7777")]
fn a_file_mode_beyond_the_permission_bits_panics_which_stops_a_const_build() {
    let _ = FileMode::new(0o10000);
}
