// SPDX-License-Identifier: GPL-3.0-or-later
use std::cell::RefCell;
use std::collections::BTreeMap;

use super::*;

/// A mise whose missing tools per folder are set up front; an install of a folder clears them
/// unless the folder is set to fail.
#[derive(Default)]
struct FakeMise {
    missing: RefCell<BTreeMap<PathBuf, BTreeSet<String>>>,
    fails: BTreeMap<PathBuf, &'static str>,
    unreadable: BTreeSet<PathBuf>,
    installs: RefCell<Vec<PathBuf>>,
}

impl FakeMise {
    fn with(missing: &[(&str, &[&str])]) -> Self {
        let fake = Self::default();
        for (folder, tools) in missing {
            fake.missing.borrow_mut().insert(
                PathBuf::from(folder),
                tools.iter().map(|t| (*t).to_owned()).collect(),
            );
        }
        fake
    }
}

impl Mise for FakeMise {
    fn missing(&self, folder: &Path) -> Result<BTreeSet<String>, MiseFailed> {
        if self.unreadable.contains(folder) {
            return Err(MiseFailed("mise ERROR error parsing config file".into()));
        }
        Ok(self
            .missing
            .borrow()
            .get(folder)
            .cloned()
            .unwrap_or_default())
    }

    fn install(&self, folder: &Path) -> Result<(), MiseFailed> {
        self.installs.borrow_mut().push(folder.to_path_buf());
        if let Some(said) = self.fails.get(folder) {
            return Err(MiseFailed((*said).into()));
        }
        self.missing.borrow_mut().remove(folder);
        Ok(())
    }
}

fn set(tools: &[&str]) -> BTreeSet<String> {
    tools.iter().map(|t| (*t).to_owned()).collect()
}

fn paths(folders: &[&str]) -> Vec<PathBuf> {
    folders.iter().map(PathBuf::from).collect()
}

#[test]
fn the_request_takes_a_folder_and_an_optional_report_flag() {
    assert_eq!(
        Request::parse(&["/workspaces/a"]),
        Ok(Request {
            root: PathBuf::from("/workspaces/a"),
            report_only: false
        })
    );
    let report = Request::parse(&["/workspaces/a", "--missing"]).unwrap();
    assert!(report.report_only);
    assert_eq!(
        report.binary(),
        Path::new("/workspaces/a").join(".puddle/mise/bin/mise")
    );
    for bad in [
        &[][..],
        &[""],
        &["/workspaces/a", "--all"],
        &["", "--missing"],
        &["a", "--missing", "x"],
    ] {
        assert_eq!(Request::parse(bad), Err(USAGE.to_owned()), "{bad:?}");
    }
}

#[test]
fn nothing_missing_installs_nothing_and_reports_only_the_end() {
    let mise = FakeMise::with(&[]);
    let report = run(&mise, &paths(&["/w/a", "/w/a/api"]), false);
    assert!(mise.installs.borrow().is_empty());
    assert!(report.is_ok());
    assert_eq!(report.render(), "end\n");
}

#[test]
fn only_folders_that_miss_a_tool_are_installed_and_the_report_says_what_landed() {
    let mise = FakeMise::with(&[("/w/a/api", &["node@22.11.0", "python@3.13.1"])]);
    let report = run(&mise, &paths(&["/w/a", "/w/a/api"]), false);
    assert_eq!(*mise.installs.borrow(), paths(&["/w/a/api"]));
    assert!(report.is_ok());
    assert_eq!(
        report.render(),
        "missing node@22.11.0\nmissing python@3.13.1\ninstalled node@22.11.0\ninstalled python@3.13.1\nend\n"
    );
}

#[test]
fn a_failed_folder_does_not_stop_the_others_and_keeps_mises_error() {
    let mut mise = FakeMise::with(&[("/w/a", &["go@1.23.0"]), ("/w/a/web", &["node@22.11.0"])]);
    mise.fails.insert(
        PathBuf::from("/w/a"),
        "mise ERROR HTTP status client error (403 Forbidden) for url (https://dl.google.com/go)",
    );
    let report = run(&mise, &paths(&["/w/a", "/w/a/web"]), false);
    assert_eq!(*mise.installs.borrow(), paths(&["/w/a", "/w/a/web"]));
    assert!(!report.is_ok());
    assert_eq!(report.installed, set(&["node@22.11.0"]));
    assert_eq!(report.failed, set(&["go@1.23.0"]));
    assert!(report.error.unwrap().0.contains("403 Forbidden"));
}

#[test]
fn report_only_lists_the_missing_tools_and_installs_nothing() {
    let mise = FakeMise::with(&[("/w/a", &["node@22.11.0"])]);
    let report = run(&mise, &paths(&["/w/a"]), true);
    assert!(mise.installs.borrow().is_empty());
    assert_eq!(report.render(), "missing node@22.11.0\nend\n");
}

#[test]
fn a_folder_mise_cannot_read_is_an_error_and_the_others_still_install() {
    let mut mise = FakeMise::with(&[("/w/a/web", &["node@22.11.0"])]);
    mise.unreadable.insert(PathBuf::from("/w/a/bad"));
    let report = run(&mise, &paths(&["/w/a/bad", "/w/a/web"]), false);
    assert_eq!(report.installed, set(&["node@22.11.0"]));
    assert!(!report.is_ok());
    assert!(report.error.unwrap().0.contains("parsing config"));
}

#[test]
fn mises_json_lists_each_version_of_each_tool() {
    let json = r#"{
      "jq": [{"version": "1.6", "requested_version": "1.6", "installed": false,
              "source": {"type": "mise.toml", "path": "/w/a/mise.toml"}}],
      "node": [{"version": "22.11.0"}, {"version": "20.18.0"}]
    }"#;
    assert_eq!(
        parse_missing(json).unwrap(),
        set(&["jq@1.6", "node@20.18.0", "node@22.11.0"])
    );
    assert_eq!(parse_missing("{}").unwrap(), BTreeSet::new());
    assert!(parse_missing("[]").is_err());
    assert!(parse_missing("mise WARN no config").is_err());
}

#[test]
fn config_folders_are_the_root_and_the_checkouts_with_a_mise_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("mise.toml"), "[tools]\n").unwrap();
    for (folder, file) in [
        ("web", ".tool-versions"),
        ("api", ".config/mise.toml"),
        ("docs", "README.md"),
        (".puddle", "mise.toml"),
        ("lib", ".mise.toml"),
        ("ops", "mise.local.toml"),
    ] {
        let path = root.join(folder).join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    std::fs::write(root.join("notes"), "a file, not a folder").unwrap();
    assert_eq!(
        config_folders(root),
        vec![
            root.to_path_buf(),
            root.join("api"),
            root.join("lib"),
            root.join("ops"),
            root.join("web"),
        ]
    );
    std::fs::remove_file(root.join("mise.toml")).unwrap();
    assert_eq!(config_folders(root).first(), Some(&root.join("api")));
    assert!(config_folders(&root.join("absent")).is_empty());
}

#[cfg(unix)]
#[test]
fn the_system_mise_reads_the_missing_tools_and_quotes_its_last_error_line() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("mise");
    // A stand-in for mise: `ls` prints one missing tool, `install` fails with an error line.
    puddle_test_kit::write_executable(
        &fake,
        "#!/bin/sh\ncase \"$1\" in\n  ls) echo '{\"node\":[{\"version\":\"22.11.0\"}]}' ;;\n  install) [ \"$MISE_SAFE\" = 1 ] || exit 9; echo 'mise ERROR no route to nodejs.org' >&2; echo >&2; exit 1 ;;\nesac\n",
    );
    let mise = SystemMise::new(fake);
    assert_eq!(mise.missing(dir.path()).unwrap(), set(&["node@22.11.0"]));
    assert_eq!(
        mise.install(dir.path()),
        Err(MiseFailed("mise ERROR no route to nodejs.org".into()))
    );
    let absent = SystemMise::new(dir.path().join("absent"));
    assert!(
        absent
            .missing(dir.path())
            .unwrap_err()
            .0
            .contains("could not run mise")
    );
    assert!(
        absent
            .install(dir.path())
            .unwrap_err()
            .0
            .contains("could not run mise")
    );
}

#[cfg(unix)]
#[test]
fn a_mise_that_fails_silently_is_named_by_its_exit_status() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("mise");
    puddle_test_kit::write_executable(&fake, "#!/bin/sh\nexit 3\n");
    let mise = SystemMise::new(fake);
    assert!(
        mise.missing(dir.path())
            .unwrap_err()
            .0
            .contains("mise ls ended")
    );
    assert!(
        mise.install(dir.path())
            .unwrap_err()
            .0
            .contains("mise install ended")
    );
}
