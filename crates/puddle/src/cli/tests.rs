// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn version_flags() {
    assert_eq!(parse(["--version"]), Ok(Command::Version));
    assert_eq!(parse(["-V"]), Ok(Command::Version));
}

#[test]
fn help_flags() {
    assert_eq!(parse(["--help"]), Ok(Command::Help));
    assert_eq!(parse(["-h"]), Ok(Command::Help));
}

#[test]
fn no_arguments_is_an_error() {
    assert_eq!(parse(Vec::<String>::new()), Err(UsageError::Missing));
}

#[test]
fn unknown_and_extra_arguments_are_errors() {
    assert_eq!(
        parse(["--bogus"]),
        Err(UsageError::UnknownArgument("--bogus".into()))
    );
    assert_eq!(
        parse(["--version", "extra"]),
        Err(UsageError::UnknownArgument("extra".into()))
    );
}
