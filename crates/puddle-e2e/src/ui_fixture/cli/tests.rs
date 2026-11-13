// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn parsed(args: &[&str]) -> Result<Option<Args>, String> {
    parse(args.iter().map(|a| (*a).to_owned()))
}

#[test]
fn the_old_serve_ui_arguments_still_parse() {
    let args = parsed(&["4173", "c.json", "--seed", "500", "--control", "dir"])
        .unwrap()
        .unwrap();
    assert_eq!(args.port, 4173);
    assert_eq!(args.connection_file, Some(PathBuf::from("c.json")));
    assert_eq!(args.seed, 500);
    assert_eq!(args.drop_dir, Some(PathBuf::from("dir")));
    assert_eq!(args.scenario, "default");
}

#[test]
fn options_are_named() {
    let args = parsed(&[
        "--port",
        "9",
        "--connection-file",
        "f",
        "--scenario",
        "lived-in",
        "--control-port",
        "10",
    ])
    .unwrap()
    .unwrap();
    assert_eq!((args.port, args.control_port), (9, 10));
    assert_eq!(args.scenario, "lived-in");
    assert_eq!(parsed(&[]).unwrap().unwrap().port, 0);
}

#[test]
fn help_and_mistakes_are_told_apart() {
    assert_eq!(parsed(&["--help"]).unwrap(), None);
    assert!(usage().contains("lived-in"));
    for bad in [
        &["--nope"][..],
        &["--port"],
        &["--port", "x"],
        &["--seed", "-1"],
        &["one"],
        &["1", "f", "extra"],
    ] {
        assert!(parsed(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_control_file_appears_whole_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let connection = dir.path().join("connection.json");
    write_control_file(&connection, "127.0.0.1:4000").unwrap();
    let control = dir.path().join("connection.json.control");
    assert_eq!(
        std::fs::read_to_string(&control).unwrap(),
        "{\"url\":\"http://127.0.0.1:4000\"}\n"
    );
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from("connection.json.control")]);
    // A directory that is gone is an error that names the file, not a panic.
    let missing = dir.path().join("gone").join("connection.json");
    let err = write_control_file(&missing, "127.0.0.1:4000").unwrap_err();
    assert!(err.contains("connection.json.control"), "{err}");
}
