// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration test: `puddle-agent mise-tools` as the host runs it. The workspace's mise is a
//! script that appends a line to the file it is given for `config set` and removes it for `unuse`, so the
//! subcommand's output, exit codes and edits come from the real binary without a network.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns, only in tests"
)]

use std::path::Path;
use std::process::{Command, Output};

fn workspace() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("puddle-agent-mise-tools-")
        .tempdir()
        .unwrap();
    let dir = root.path().display();
    std::fs::write(
        root.path().join("mise.toml"),
        "# mine\n[tools]\nnode = \"22\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join(".puddle/mise/bin")).unwrap();
    puddle_test_kit::write_executable(
        &root.path().join(".puddle/mise/bin/mise"),
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n  ls) echo '{{\"node\":[{{\"requested_version\":\"22\",\"installed\":true,\"source\":{{\"path\":\"{dir}/mise.toml\"}}}}]}}' ;;\n  config) echo \"$5=$6\" >> \"$4\" ;;\n  unuse) grep -vF \"tools.\\\"$5\\\"\" \"$4\" > \"{dir}/tmp\"; cat \"{dir}/tmp\" > \"$4\" ;;\n  registry) echo '[{{\"short\":\"node\",\"description\":\"JS\"}}]' ;;\n  ls-remote) [ \"$2\" = silent ] && exit 3; [ \"$2\" = broken ] && {{ echo 'mise ERROR no route to mise-versions.jdx.dev' >&2; exit 1; }}; printf '20.0.0\\n22.0.0\\n' ;;\nesac\n"
        ),
    );
    root
}

fn mise_tools(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_puddle-agent"))
        .arg("mise-tools")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn hash_of(listing: &str) -> String {
    listing
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("hash\t"))
        .unwrap()
        .to_owned()
}

#[test]
fn a_listing_then_an_edit_with_the_listed_hash_changes_only_that_file() {
    let root = workspace();
    let listing = mise_tools(root.path(), &["list"]);
    assert!(listing.status.success(), "{listing:?}");
    let listing = text(&listing.stdout);
    assert!(
        listing.ends_with("tool\tmise.toml\tnode\t22\t1\nend\n"),
        "{listing}"
    );
    let hash = hash_of(&listing);

    let set = mise_tools(root.path(), &["set", "go", "1.23", &hash]);
    assert!(set.status.success(), "{set:?}");
    let new_hash = hash_of(&text(&set.stdout));
    assert_ne!(new_hash, hash);
    let file = std::fs::read_to_string(root.path().join("mise.toml")).unwrap();
    assert!(
        file.starts_with("# mine\n") && file.ends_with("tools.\"go\"=1.23\n"),
        "{file}"
    );

    let stale = mise_tools(root.path(), &["remove", "go", &hash]);
    assert_eq!(stale.status.code(), Some(5), "{stale:?}");
    assert!(text(&stale.stderr).contains("changed since it was listed"));
    let removed = mise_tools(root.path(), &["remove", "go", &new_hash]);
    assert!(removed.status.success(), "{removed:?}");
    let file = std::fs::read_to_string(root.path().join("mise.toml")).unwrap();
    assert_eq!(file, "# mine\n[tools]\nnode = \"22\"\n");
}

#[test]
fn the_registry_and_the_versions_are_records_and_a_failure_names_its_reason() {
    let root = workspace();
    let registry = mise_tools(root.path(), &["registry"]);
    assert_eq!(text(&registry.stdout), "reg\tnode\tJS\nend\n");
    let versions = mise_tools(root.path(), &["versions", "node"]);
    assert_eq!(text(&versions.stdout), "ver\t22.0.0\nver\t20.0.0\nend\n");
    let broken = mise_tools(root.path(), &["versions", "broken"]);
    assert_eq!(broken.status.code(), Some(1), "{broken:?}");
    assert_eq!(text(&broken.stdout), "");
    assert_eq!(
        text(&broken.stderr).trim(),
        "mise ERROR no route to mise-versions.jdx.dev"
    );
    let silent = mise_tools(root.path(), &["versions", "silent"]);
    assert_eq!(silent.status.code(), Some(1), "{silent:?}");
    assert!(
        text(&silent.stderr).starts_with("mise ls-remote ended with exit status: 3"),
        "{silent:?}"
    );
}

#[test]
fn a_bad_command_line_is_usage_and_exit_code_2() {
    let root = workspace();
    let out = mise_tools(root.path(), &["set", "-x", "1", "absent"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}
