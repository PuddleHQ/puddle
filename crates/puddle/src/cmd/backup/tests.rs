// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::cmd::serve::options::Value;

fn flags(pairs: &[(&'static str, &str)]) -> Flags {
    pairs
        .iter()
        .map(|(key, path)| (*key, Value::Path((*path).into())))
        .collect()
}

#[test]
fn destination_alone_and_with_the_data_folder_options() {
    assert_eq!(
        parse(["out"].into_iter()),
        Ok(Command::Backup(BackupArgs {
            dest: "out".into(),
            flags: Flags::new()
        }))
    );
    assert_eq!(
        parse(["--data-dir", "d", "--config", "c.toml", "out"].into_iter()),
        Ok(Command::Backup(BackupArgs {
            dest: "out".into(),
            flags: flags(&[("data_dir", "d"), ("config", "c.toml")])
        }))
    );
}

#[test]
fn missing_or_extra_arguments_are_errors() {
    assert_eq!(
        parse(Vec::<&str>::new().into_iter()),
        Err(UsageError::MissingArgument("backup <dest>"))
    );
    assert_eq!(
        parse(["a", "b"].into_iter()),
        Err(UsageError::UnknownArgument("b".into()))
    );
    assert_eq!(
        parse(["--bogus"].into_iter()),
        Err(UsageError::UnknownArgument("--bogus".into()))
    );
    assert_eq!(
        parse(["--data-dir"].into_iter()),
        Err(UsageError::MissingArgument("--data-dir <path>"))
    );
}

fn no_env(_: &str) -> Option<OsString> {
    None
}

fn args(flags: Flags, dest: &Path) -> BackupArgs {
    BackupArgs {
        dest: dest.to_path_buf(),
        flags,
    }
}

/// A data folder with one database and nothing else, as a fresh host leaves it.
fn data_folder_with_a_database(root: &Path) -> PathBuf {
    let data = root.join("data");
    let paths = HostPaths::new(&data);
    std::fs::create_dir_all(&data).unwrap();
    drop(
        puddle_store::Store::open(
            &paths.store(),
            std::sync::Arc::new(puddle_store::SystemClock),
            puddle_store::Limits::default(),
        )
        .unwrap(),
    );
    data
}

#[test]
fn no_per_user_folder_is_an_error_unless_an_option_names_one() {
    let root = tempfile::tempdir().unwrap();
    let err = execute(
        &args(Flags::new(), &root.path().join("out")),
        &no_env,
        root.path(),
        Err(puddle_fs::DataDirError::NoBase),
    )
    .unwrap_err();
    assert!(err.contains("per-user data folder"), "{err}");
}

#[test]
fn the_environment_variable_names_the_folder_serve_would_use_not_the_per_user_one() {
    let root = tempfile::tempdir().unwrap();
    let data = data_folder_with_a_database(root.path());
    let wrong = root.path().join("per-user");
    std::fs::create_dir_all(&wrong).unwrap();
    let dest = root.path().join("out");
    let env = |name: &str| (name == "PUDDLE_DATA_DIR").then(|| data.clone().into_os_string());
    let report = execute(&args(Flags::new(), &dest), &env, root.path(), Ok(wrong)).unwrap();
    assert_eq!(report.dest, dest);
    assert!(dest.join("puddle.db").is_file());
}

#[test]
fn a_config_file_names_the_folder_and_a_flag_beats_the_environment() {
    let root = tempfile::tempdir().unwrap();
    let data = data_folder_with_a_database(root.path());
    let config = root.path().join("serve.toml");
    std::fs::write(
        &config,
        format!("data_dir = {:?}\n", data.to_str().unwrap()),
    )
    .unwrap();
    let dest = root.path().join("from-file");
    let via_file = args(flags(&[("config", config.to_str().unwrap())]), &dest);
    execute(
        &via_file,
        &no_env,
        root.path(),
        Err(puddle_fs::DataDirError::NoBase),
    )
    .unwrap();
    assert!(dest.join("puddle.db").is_file());

    // The flag wins over a variable that points at a folder with no database.
    let empty = root.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let env = |name: &str| (name == "PUDDLE_DATA_DIR").then(|| empty.clone().into_os_string());
    let dest = root.path().join("from-flag");
    let via_flag = args(flags(&[("data_dir", data.to_str().unwrap())]), &dest);
    execute(
        &via_flag,
        &env,
        root.path(),
        Err(puddle_fs::DataDirError::NoBase),
    )
    .unwrap();
    assert!(dest.join("puddle.db").is_file());
}

#[test]
fn a_relative_data_dir_means_the_working_folder() {
    let root = tempfile::tempdir().unwrap();
    data_folder_with_a_database(root.path());
    let dest = root.path().join("out");
    execute(
        &args(flags(&[("data_dir", "data")]), &dest),
        &no_env,
        root.path(),
        Err(puddle_fs::DataDirError::NoBase),
    )
    .unwrap();
    assert!(dest.join("puddle.db").is_file());
}

#[test]
fn an_unusable_config_file_is_an_error_not_a_backup_of_the_wrong_folder() {
    let root = tempfile::tempdir().unwrap();
    let wrong = data_folder_with_a_database(root.path());
    let dest = root.path().join("out");
    let err = execute(
        &args(flags(&[("config", "missing.toml")]), &dest),
        &no_env,
        root.path(),
        Ok(wrong),
    )
    .unwrap_err();
    assert!(err.contains("missing.toml"), "{err}");
    assert!(!dest.exists());
}
