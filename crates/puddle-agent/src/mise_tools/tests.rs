// SPDX-License-Identifier: GPL-3.0-or-later
use std::cell::RefCell;

use super::*;

/// A mise that answers by its first argument and records every call.
struct FakeMise {
    ls: String,
    registry: String,
    remote: String,
    fail: Option<&'static str>,
    calls: RefCell<Vec<Vec<String>>>,
}

impl FakeMise {
    fn new(ls: &str) -> Self {
        Self {
            ls: ls.to_owned(),
            registry: "[]".to_owned(),
            remote: String::new(),
            fail: None,
            calls: RefCell::default(),
        }
    }
}

impl Mise for FakeMise {
    fn run(&self, _dir: &Path, args: &[&str]) -> Result<String, MiseFailed> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|a| (*a).to_owned()).collect());
        if let Some(why) = self.fail {
            return Err(MiseFailed(why.to_owned()));
        }
        Ok(match args.first().copied() {
            Some("ls") => self.ls.clone(),
            Some("registry") => self.registry.clone(),
            Some("ls-remote") => self.remote.clone(),
            _ => String::new(),
        })
    }
}

fn request(root: &Path, operation: Operation) -> Request {
    Request {
        root: root.to_path_buf(),
        operation,
    }
}

fn name(n: &str) -> ToolName {
    ToolName::new(n).unwrap()
}

fn workspace() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("puddle-agent-mise-tools-")
        .tempdir()
        .unwrap()
}

fn entry(tool: &str, version: &str, installed: bool, path: &str) -> String {
    format!(
        r#""{tool}":[{{"requested_version":"{version}","installed":{installed},"source":{{"type":"mise.toml","path":"{path}"}}}}]"#
    )
}

#[test]
fn the_command_line_is_checked_before_anything_runs() {
    let ok = Request::parse(&["/w", "set", "node", "22", "absent"]).unwrap();
    assert_eq!(
        ok.operation,
        Operation::Set(
            name("node"),
            ToolVersion::new("22").unwrap(),
            ToolsFileHash::absent()
        )
    );
    assert_eq!(ok.binary(), PathBuf::from("/w/.puddle/mise/bin/mise"));
    assert_eq!(
        Request::parse(&["/w", "remove", "node", "absent"])
            .unwrap()
            .operation,
        Operation::Remove(name("node"), ToolsFileHash::absent())
    );
    assert_eq!(
        Request::parse(&["/w", "versions", "node"])
            .unwrap()
            .operation,
        Operation::Versions(name("node"))
    );
    assert_eq!(
        Request::parse(&["/w", "registry"]).unwrap().operation,
        Operation::Registry
    );
    for bad in [
        &[][..],
        &["/w"],
        &["", "list"],
        &["/w", "list", "x"],
        &["/w", "set", "node", "22"],
        &["/w", "frobnicate"],
    ] {
        assert_eq!(Request::parse(bad).unwrap_err(), USAGE, "{bad:?}");
    }
    for bad in [
        &["/w", "set", "-x", "22", "absent"][..],
        &["/w", "set", "node", "--yes", "absent"],
        &["/w", "set", "node", "22", "nope"],
        &["/w", "remove", "a b", "absent"],
        &["/w", "versions", "a\"b"],
    ] {
        let err = Request::parse(bad).unwrap_err();
        assert!(err.ends_with(USAGE) && err.starts_with("invalid"), "{err}");
    }
}

#[test]
fn a_listing_has_the_root_hash_and_the_tools_of_the_workspaces_own_files_only() {
    let dir = workspace();
    let root = dir.path();
    std::fs::write(root.join("mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();
    std::fs::create_dir(root.join("api")).unwrap();
    std::fs::write(root.join("api/.tool-versions"), "python 3.12\n").unwrap();
    let at = |rel: &str| root.join(rel).display().to_string();
    let ls = format!(
        "{{{},{},{},{},{},{},{},{}}}",
        entry("node", "22", true, &at("mise.toml")),
        entry("python", "3.12", false, &at("api/.tool-versions")),
        entry("go", "1.23", false, "/home/dev/.config/mise/config.toml"),
        entry("zig", "0.13", false, &at(".puddle/mise/config.toml")),
        entry("bad name", "1", false, &at("mise.toml")),
        entry("ruby", "-x", false, &at("mise.toml")),
        entry("rust", "stable", true, &at("notes.txt")),
        r#""deno":[{"requested_version":"2","installed":false}]"#,
    );
    let mise = FakeMise::new(&ls);
    let out = run(&mise, &request(root, Operation::List)).unwrap();
    let hash = sha256_of(&root.join("mise.toml")).unwrap();
    assert_eq!(
        out,
        format!(
            "hash\t{hash}\ntool\tmise.toml\tnode\t22\t1\ntool\tapi/.tool-versions\tpython\t3.12\t0\nend\n"
        )
    );
    // One `ls` per folder with a config: the root and the checkout.
    assert_eq!(mise.calls.borrow().len(), 2);
}

#[test]
fn a_workspace_with_no_file_lists_absent() {
    let dir = workspace();
    let out = run(&FakeMise::new("{}"), &request(dir.path(), Operation::List)).unwrap();
    assert_eq!(out, "hash\tabsent\nend\n");
}

#[test]
fn an_unreadable_listing_is_mises_failure() {
    let dir = workspace();
    std::fs::write(dir.path().join("mise.toml"), "").unwrap();
    let err = run(&FakeMise::new("[1]"), &request(dir.path(), Operation::List)).unwrap_err();
    assert!(
        matches!(&err, Failure::Mise(m) if m.0.contains("unexpected")),
        "{err}"
    );
    let mut broken = FakeMise::new("{}");
    broken.fail = Some("mise ERROR error parsing config file");
    let err = run(&broken, &request(dir.path(), Operation::List)).unwrap_err();
    assert_eq!(err.to_string(), "mise ERROR error parsing config file");
}

#[test]
fn a_set_checks_the_hash_creates_the_file_and_quotes_the_key() {
    let dir = workspace();
    let root = dir.path();
    let version = ToolVersion::new("22").unwrap();
    let set = |hash: ToolsFileHash| Operation::Set(name("npm:prettier"), version.clone(), hash);
    let mise = FakeMise::new("{}");
    let out = run(&mise, &request(root, set(ToolsFileHash::absent()))).unwrap();
    assert_eq!(
        out,
        "hash\tabsent\nend\n".replace("absent", &sha256_of(&root.join("mise.toml")).unwrap())
    );
    let file = root.join("mise.toml").display().to_string();
    assert_eq!(
        *mise.calls.borrow(),
        [["config", "set", "-f", &file, "tools.\"npm:prettier\"", "22"]]
    );
    // The file exists now, so `absent` is stale.
    let again = run(&mise, &request(root, set(ToolsFileHash::absent())));
    assert_eq!(again, Err(Failure::Changed));
    assert_eq!(
        mise.calls.borrow().len(),
        1,
        "a refused edit asks mise nothing"
    );
}

#[test]
fn a_remove_checks_the_hash_and_uses_unuse_without_pruning() {
    let dir = workspace();
    let root = dir.path();
    std::fs::write(root.join("mise.toml"), "[tools]\nnode = \"22\"\n").unwrap();
    let hash = ToolsFileHash::new(&sha256_of(&root.join("mise.toml")).unwrap()).unwrap();
    let mise = FakeMise::new("{}");
    let stale = run(
        &mise,
        &request(
            root,
            Operation::Remove(name("node"), ToolsFileHash::absent()),
        ),
    );
    assert_eq!(stale, Err(Failure::Changed));
    assert!(mise.calls.borrow().is_empty());
    run(&mise, &request(root, Operation::Remove(name("node"), hash))).unwrap();
    let file = root.join("mise.toml").display().to_string();
    assert_eq!(
        *mise.calls.borrow(),
        [["unuse", "--no-prune", "-p", &file, "node"]]
    );
}

#[test]
fn a_set_that_cannot_create_the_file_or_read_it_says_so() {
    let dir = workspace();
    let missing = dir.path().join("no-such-folder");
    let set = Operation::Set(
        name("node"),
        ToolVersion::new("22").unwrap(),
        ToolsFileHash::absent(),
    );
    let err = run(&FakeMise::new("{}"), &request(&missing, set)).unwrap_err();
    assert!(
        err.to_string().starts_with("could not create mise.toml"),
        "{err}"
    );
    // A folder where the file should be: reading it fails with something other than "not found".
    std::fs::create_dir(dir.path().join("mise.toml")).unwrap();
    let err = run(&FakeMise::new("{}"), &request(dir.path(), Operation::List)).unwrap_err();
    assert!(
        err.to_string().starts_with("could not read mise.toml"),
        "{err}"
    );
}

#[test]
fn the_registry_is_names_and_one_line_descriptions() {
    let dir = workspace();
    let mut mise = FakeMise::new("{}");
    let long = "x".repeat(300);
    mise.registry = format!(
        r#"[{{"short":"node","description":"Node.js\nruntime"}},{{"short":"bad name","description":"x"}},{{"short":"go"}},{{"short":"long","description":"{long}"}}]"#
    );
    let out = run(&mise, &request(dir.path(), Operation::Registry)).unwrap();
    assert_eq!(
        out,
        format!(
            "reg\tnode\tNode.js runtime\nreg\tgo\t\nreg\tlong\t{}\nend\n",
            "x".repeat(MAX_DESCRIPTION)
        )
    );
    mise.registry = "{}".to_owned();
    let err = run(&mise, &request(dir.path(), Operation::Registry)).unwrap_err();
    assert!(
        err.to_string()
            .contains("registry printed something unexpected"),
        "{err}"
    );
}

#[test]
fn the_versions_are_the_newest_valid_lines_newest_first() {
    let dir = workspace();
    let mut mise = FakeMise::new("{}");
    for n in 0..300 {
        let _ = writeln!(mise.remote, "1.{n}.0");
    }
    mise.remote.push_str("bad version\n\n");
    let out = run(
        &mise,
        &request(dir.path(), Operation::Versions(name("node"))),
    )
    .unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), MAX_VERSIONS + 1);
    assert_eq!(lines.first(), Some(&"ver\t1.299.0"));
    assert_eq!(lines.get(MAX_VERSIONS - 1), Some(&"ver\t1.100.0"));
    assert_eq!(lines.last(), Some(&"end"));
    assert_eq!(*mise.calls.borrow(), [["ls-remote", "node"]]);
}
