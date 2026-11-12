// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::cmd::serve::options::Value;

fn take(args: &[&str]) -> Result<Flags, UsageError> {
    let mut rest = args.iter().copied();
    let mut flags = Flags::new();
    while let Some(arg) = rest.next() {
        if !take_option(arg, &mut rest, &mut flags)? {
            return Err(UsageError::UnknownArgument(arg.to_owned()));
        }
    }
    Ok(flags)
}

#[test]
fn only_the_two_data_folder_options_are_taken() {
    let flags = take(&["--data-dir", "d", "--config", "c.toml"]).unwrap();
    assert_eq!(flags.get("data_dir"), Some(&Value::Path("d".into())));
    assert_eq!(flags.get("config"), Some(&Value::Path("c.toml".into())));
    // Other serve options are not these commands' business.
    assert_eq!(
        take(&["--port", "80"]),
        Err(UsageError::UnknownArgument("--port".into()))
    );
}

#[test]
fn a_missing_value_or_a_repeat_is_a_usage_error() {
    assert_eq!(
        take(&["--config"]),
        Err(UsageError::MissingArgument("--config <path>"))
    );
    assert_eq!(
        take(&["--config", "a", "--config", "b"]),
        Err(UsageError::UnknownArgument("--config".into()))
    );
}

#[test]
fn no_option_and_no_variable_leaves_the_choice_to_the_per_user_default() {
    let found = configured(&Flags::new(), &|_| None, Path::new("/w")).unwrap();
    assert_eq!(found, None);
}

#[test]
fn a_bad_variable_is_an_error_naming_it() {
    let err = configured(
        &Flags::new(),
        &|name| (name == "PUDDLE_DATA_DIR").then(|| OsString::from("")),
        Path::new("/w"),
    )
    .unwrap_err();
    assert!(err.to_string().contains("PUDDLE_DATA_DIR"), "{err}");
}

#[test]
fn an_empty_path_is_a_usage_error_naming_the_option() {
    let err = take(&["--data-dir", ""]).unwrap_err();
    assert!(
        matches!(&err, UsageError::InvalidValue { option, .. } if *option == "--data-dir"),
        "{err:?}"
    );
}
