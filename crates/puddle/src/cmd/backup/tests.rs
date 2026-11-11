// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

#[test]
fn destination_alone_and_with_a_data_dir() {
    assert_eq!(
        parse(["out"].into_iter()),
        Ok(Command::Backup(BackupArgs {
            dest: "out".into(),
            data_dir: None
        }))
    );
    assert_eq!(
        parse(["--data-dir", "d", "out"].into_iter()),
        Ok(Command::Backup(BackupArgs {
            dest: "out".into(),
            data_dir: Some("d".into())
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

#[test]
fn no_per_user_folder_is_an_error_unless_a_data_dir_is_given() {
    let args = BackupArgs {
        dest: "out".into(),
        data_dir: None,
    };
    let err = execute(&args, Err(puddle_fs::DataDirError::NoBase)).unwrap_err();
    assert!(err.contains("per-user data folder"), "{err}");
}
