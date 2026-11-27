// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

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

/// The table is the parents of the list `psl` is built from. After a `psl` bump this fails
/// until `UPDATE_PSL_PARENTS=1 cargo test -p puddle-proxy --lib public_suffix_parents_match`
/// rewrites the file.
#[test]
fn public_suffix_parents_match_the_psl_crate_rules() {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--offline"])
        .arg("--manifest-path")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let manifest = meta["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "psl")
        .and_then(|p| p["manifest_path"].as_str())
        .expect("psl is a dependency");
    let rules =
        std::fs::read_to_string(Path::new(manifest).parent().unwrap().join("data/rules.txt"))
            .unwrap();
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
