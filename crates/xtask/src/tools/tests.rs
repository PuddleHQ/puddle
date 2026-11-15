// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn git_command_drops_every_repository_variable_git_lists() {
    let listed = run(Command::new("git").args(["rev-parse", "--local-env-vars"])).unwrap();
    let cmd = git_command();
    let removed: BTreeSet<_> = cmd
        .get_envs()
        .filter(|(_, v)| v.is_none())
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    for var in listed.split_whitespace() {
        assert!(removed.contains(var), "{var} is not stripped");
    }
    assert!(!removed.contains("PATH"));
}

#[test]
fn feature_args() {
    assert_eq!(Features::All.args(), ["--all-features"]);
    assert_eq!(
        Features::Only(vec!["a".into(), "b".into()]).args(),
        ["--no-default-features", "--features", "a b"]
    );
}

#[test]
fn run_reports_failures_and_missing_programs() {
    let err = run(&mut Command::new("/nonexistent/xtask-tool"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("cannot start"), "{err}");
    assert!(err.contains("install it and run the task again"), "{err}");
    let cargo = Cargo::from_env(true);
    let err = run(cargo
        .command()
        .args(["metadata", "--manifest-path", "/nonexistent/Cargo.toml"]))
    .unwrap_err()
    .to_string();
    assert!(err.contains("failed"), "{err}");
    let ok = run(cargo.command().arg("--version")).unwrap();
    assert!(ok.starts_with("cargo "), "{ok}");
}

// `--offline` only finds sources already downloaded, and a build fetches only the crates its
// own target needs: a fixed foreign triple fails on a host that never built for it (Windows
// CI lacked the Linux-only `caps`). The host's tree is what this test run just built.
const HOST: &str = "host-tuple";

#[test]
fn shipped_reads_this_workspace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let shipped = Cargo::from_env(true)
        .shipped(&root, &Features::All, &[HOST], true)
        .unwrap();
    assert!(shipped.iter().any(|p| p.name == "thiserror"));
    assert!(
        shipped.iter().all(|p| !p.name.starts_with("puddle")),
        "own crates are local"
    );
}

#[cfg(unix)]
#[test]
fn inventory_asks_tree_and_about_for_the_same_targets() {
    // A fake cargo: `tree` lists one package, `about` reports it under MIT; both log their
    // arguments so the test sees which targets each was asked for.
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("args.log");
    let script = dir.path().join("cargo");
    let report = r#"{"licenses":[{"name":"MIT","id":"MIT","text":"t","used_by":[{"crate":{"name":"dep","version":"1.0.0","repository":null},"path":""}]}]}"#;
    puddle_test_kit::write_executable(
        &script,
        &format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1\" in\n  tree) echo 'dep v1.0.0' ;;\n  about) echo '{report}' ;;\nesac\n",
            log.display()
        ),
    );
    let cargo = Cargo {
        program: script.into_os_string(),
        offline: false,
    };
    cargo
        .inventory(
            "t",
            Path::new("m/Cargo.toml"),
            Path::new("c.toml"),
            &Features::All,
            &["a-linux", "b-windows"],
            false,
        )
        .unwrap()
        .check()
        .unwrap();
    let log = std::fs::read_to_string(log).unwrap();
    let about = log.lines().find(|l| l.starts_with("about")).unwrap();
    assert!(
        about.contains("--target a-linux --target b-windows"),
        "{about}"
    );
    assert!(log.contains("--target a-linux") && log.contains("--target b-windows"));
}

#[test]
fn dev_only_features_do_not_ship() {
    // object's `write` feature is only turned on by puddle-runtime's dev-dependencies: its
    // extra deps don't ship. Checked on that one package: elsewhere in the workspace the msb
    // SDK (puddle-vm-tests) needs crc32fast on its own.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../puddle-runtime/Cargo.toml");
    let shipped = Cargo::from_env(true)
        .shipped(&manifest, &Features::All, &[HOST], false)
        .unwrap();
    assert!(shipped.iter().any(|p| p.name == "object"));
    assert!(shipped.iter().all(|p| p.name != "crc32fast"));
}

#[cfg(unix)]
#[test]
fn run_reports_a_program_that_cannot_be_executed_without_an_install_hint() {
    let dir = tempfile::tempdir().unwrap();
    let err = run(&mut Command::new(dir.path())).unwrap_err().to_string();
    assert!(err.contains("cannot start"), "{err}");
    assert!(!err.contains("install it"), "{err}");
}
