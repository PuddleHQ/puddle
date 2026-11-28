// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{PARENTS, covers_public_suffix};

#[test]
fn registrable_bases_with_a_public_suffix_below_are_covered() {
    for base in ["amazonaws.com", "windows.net", "core.windows.net", "co.uk"] {
        assert!(covers_public_suffix(base), "{base}");
    }
    for base in ["github.com", "githubcopilot.com", "example.co.uk"] {
        assert!(!covers_public_suffix(base), "{base}");
    }
}

/// The parents of every rule in `rules`, as the file stores them.
fn parents(rules: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in rules.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let rule = line.trim_start_matches('!');
        let rule = rule.strip_prefix("*.").unwrap_or(rule);
        let labels: Vec<&str> = rule.split('.').collect();
        for from in 1..labels.len().saturating_sub(1) {
            out.insert(labels[from..].join("."));
        }
    }
    out
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The version `Cargo.lock` locks for `name`, or `None` unless it locks exactly one.
/// Reading the lock file needs no dependency resolution, unlike `cargo metadata`, which
/// wants every platform's crates in the registry.
fn locked_version(lock: &str, name: &str) -> Option<String> {
    let header = format!("name = \"{name}\"");
    let mut versions = Vec::new();
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line == header {
            let version = lines.next()?.strip_prefix("version = \"")?;
            versions.push(version.strip_suffix('"')?.to_owned());
        }
    }
    match versions.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The extracted source of `name-version` in any registry under `cargo_home`.
fn registry_source(cargo_home: &Path, name: &str, version: &str) -> Option<PathBuf> {
    std::fs::read_dir(cargo_home.join("registry/src"))
        .ok()?
        .filter_map(Result::ok)
        .map(|index| index.path().join(format!("{name}-{version}")))
        .find(|dir| dir.is_dir())
}

#[test]
fn locked_version_reads_the_single_locked_package() {
    let lock = "[[package]]\nname = \"psl-types\"\nversion = \"2.0.11\"\n\n\
                [[package]]\nname = \"psl\"\nversion = \"2.1.261\"\n";
    assert_eq!(locked_version(lock, "psl").as_deref(), Some("2.1.261"));
    assert_eq!(locked_version(lock, "psl-types").as_deref(), Some("2.0.11"));
    assert_eq!(locked_version(lock, "absent"), None);
    let twice = format!("{lock}\n[[package]]\nname = \"psl\"\nversion = \"2.1.1\"\n");
    assert_eq!(locked_version(&twice, "psl"), None);
    assert_eq!(locked_version("[[package]]\nname = \"psl\"\n", "psl"), None);
    assert_eq!(
        locked_version("[[package]]\nname = \"psl\"\nversion = 1\n", "psl"),
        None
    );
}

#[test]
fn registry_source_finds_the_exact_version_in_any_index() {
    let home = tempfile::tempdir().unwrap();
    let src = home.path().join("registry/src");
    std::fs::create_dir_all(src.join("index-a/psl-2.1.1")).unwrap();
    std::fs::create_dir_all(src.join("index-b/psl-2.1.2")).unwrap();
    std::fs::write(src.join("index-b/psl-2.1.3"), "a file, not a crate").unwrap();
    assert_eq!(
        registry_source(home.path(), "psl", "2.1.2"),
        Some(src.join("index-b/psl-2.1.2"))
    );
    assert_eq!(
        registry_source(home.path(), "psl", "2.1.1"),
        Some(src.join("index-a/psl-2.1.1"))
    );
    assert_eq!(registry_source(home.path(), "psl", "2.1.3"), None);
    assert_eq!(registry_source(home.path(), "psl", "9.9.9"), None);
    assert_eq!(
        registry_source(&home.path().join("none"), "psl", "2.1.1"),
        None
    );
}

/// The table is the parents of the list `psl` is built from. After a `psl` bump this fails
/// until `UPDATE_PSL_PARENTS=1 cargo test -p puddle-proxy --lib public_suffix_parents_match`
/// rewrites the file.
#[test]
fn public_suffix_parents_match_the_psl_crate_rules() {
    let lock = std::fs::read_to_string(workspace_root().join("Cargo.lock")).unwrap();
    let version = locked_version(&lock, "psl").expect("Cargo.lock locks exactly one psl");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")))
        .expect("CARGO_HOME or HOME is set");
    let dir = registry_source(&cargo_home, "psl", &version)
        .unwrap_or_else(|| panic!("psl-{version} is not extracted under {cargo_home:?}"));
    let rules = std::fs::read_to_string(dir.join("data/rules.txt")).unwrap();
    let want = parents(&rules);
    let mut text = want
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/terminate/stand_in/public_suffix_parents.txt");
    let have: BTreeSet<String> = PARENTS.iter().map(|p| (*p).to_owned()).collect();
    if std::env::var_os("UPDATE_PSL_PARENTS").is_some() {
        std::fs::write(file, text).unwrap();
    } else {
        assert_eq!(
            want.difference(&have).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "missing from public_suffix_parents.txt"
        );
        assert_eq!(
            have.difference(&want).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "stale in public_suffix_parents.txt"
        );
    }
}
