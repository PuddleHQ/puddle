// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

pub(crate) fn pkg(name: &str, version: &str) -> Package {
    Package {
        name: name.into(),
        version: version.into(),
    }
}

fn tree() -> String {
    "lib v1.0.0\nbuild-helper v2.0.0\nshared v4.0.0\nshared v4.0.0 (*)\nderive v1.0.0 (proc-macro)\n\
         app v0.0.0 (/work/crates/app)\nwin v0.0.0 (C:\\work\\win) (*)\n\
         git-dep v0.1.0 (https://github.com/o/r?branch=b#abc)\n"
            .to_owned()
}

fn about(crates: &[(&str, &str)]) -> String {
    let used_by: Vec<_> = crates
            .iter()
            .map(|(n, v)| serde_json::json!({"crate": {"name": n, "version": v, "repository": format!("https://example.invalid/{n}")}, "path": ""}))
            .collect();
    serde_json::json!({
        "overview": [],
        "crates": [],
        "licenses": [{"name": "MIT License", "id": "MIT", "text": "MIT text\n", "used_by": used_by}]
    })
    .to_string()
}

#[test]
fn shipped_packages_are_registry_and_git_packages() {
    let shipped = shipped_packages(&tree()).unwrap();
    let names: Vec<_> = shipped.iter().map(ToString::to_string).collect();
    assert_eq!(
        names,
        [
            "build-helper 2.0.0",
            "derive 1.0.0",
            "git-dep 0.1.0",
            "lib 1.0.0",
            "shared 4.0.0"
        ]
    );
}

#[test]
fn malformed_tree_lines_are_errors() {
    for bad in ["lonely", "name 1.0.0", "name v", "name v1.0.0\nx"] {
        assert!(shipped_packages(bad).is_err(), "{bad:?}");
    }
    assert!(shipped_packages("").unwrap().is_empty());
}

#[test]
fn a_dependency_without_licence_entry_fails_the_check() {
    let required = shipped_packages(&tree()).unwrap();
    let covered = [
        ("lib", "1.0.0"),
        ("build-helper", "2.0.0"),
        ("derive", "1.0.0"),
    ];
    let inv = Inventory::new("puddle", &about(&covered), required).unwrap();
    assert_eq!(
        inv.missing(),
        [&pkg("git-dep", "0.1.0"), &pkg("shared", "4.0.0")]
    );
    let err = inv.check().unwrap_err().to_string();
    assert_eq!(
        err,
        "no licence entry in the puddle notices for: git-dep 0.1.0, shared 4.0.0"
    );
}

#[test]
fn another_version_does_not_count() {
    let inv = Inventory::new(
        "t",
        &about(&[("lib", "1.0.1")]),
        [pkg("lib", "1.0.0")].into(),
    )
    .unwrap();
    assert_eq!(inv.missing(), [&pkg("lib", "1.0.0")]);
}

#[test]
fn full_coverage_passes_and_renders() {
    let required = shipped_packages(&tree()).unwrap();
    let inv = Inventory::new(
        "puddle",
        &about(&[
            ("lib", "1.0.0"),
            ("build-helper", "2.0.0"),
            ("derive", "1.0.0"),
            ("git-dep", "0.1.0"),
            ("shared", "4.0.0"),
            ("shared", "4.0.0"),
        ]),
        required,
    )
    .unwrap();
    inv.check().unwrap();
    assert_eq!(inv.groups().len(), 1);
    let text = inv.render("intro line");
    assert!(text.starts_with(
        "Third-party notices: puddle\n\nintro line\n\n5 packages, 1 licence texts.\n"
    ));
    assert!(text.contains("MIT License (MIT)"));
    assert!(text.contains("  - shared 4.0.0 <https://example.invalid/shared>\n"));
    assert_eq!(text.matches("shared 4.0.0").count(), 1, "deduplicated");
    assert!(text.trim_end().ends_with("MIT text"));
}

#[test]
fn renders_crates_without_repository() {
    let json = serde_json::json!({"licenses": [{"name": "N", "id": "X", "text": "T", "used_by": [{"crate": {"name": "a", "version": "1"}}]}]});
    let inv = Inventory::new("t", &json.to_string(), BTreeSet::new()).unwrap();
    assert!(inv.render("").contains("  - a 1\n"));
    assert!(Inventory::new("t", "{}", BTreeSet::new()).is_err());
}
