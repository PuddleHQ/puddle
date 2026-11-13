// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock};
use sha2::Digest as _;

use super::*;
use crate::cmd::serve::options::Value;

fn reset(args: &[&str]) -> Result<ResetArgs, UsageError> {
    match parse(args.iter()) {
        Ok(Command::UserReset(a)) => Ok(a),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

#[test]
fn reset_takes_a_name_and_the_two_options_that_find_the_server() {
    let a = reset(&["reset", "ann"]).unwrap();
    assert_eq!(a.name, "ann");
    assert!(a.flags.is_empty());
    let a = reset(&["reset", "--config", "s.toml", "ann", "--data-dir", "d"]).unwrap();
    assert_eq!(a.name, "ann");
    assert_eq!(a.flags.get("config"), Some(&Value::Path("s.toml".into())));
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
}

#[test]
fn anything_else_is_a_usage_error() {
    assert_eq!(
        reset(&[]),
        Err(UsageError::MissingArgument("user reset <name>"))
    );
    assert_eq!(
        reset(&["reset"]),
        Err(UsageError::MissingArgument("user reset <name>"))
    );
    assert_eq!(
        reset(&["delete", "ann"]),
        Err(UsageError::UnknownArgument("delete".into()))
    );
    assert!(reset(&["reset", "ann", "bob"]).is_err());
    assert!(reset(&["reset", "ann", "--port", "1"]).is_err());
    assert!(reset(&["reset", "ann", "--password", "x"]).is_err());
    assert!(reset(&["reset", "--config", "a", "--config", "b", "ann"]).is_err());
    assert_eq!(
        reset(&["reset", "ann", "--config"]),
        Err(UsageError::MissingArgument("--config <path>"))
    );
    assert!(reset(&["reset", "ann", "--data-dir", ""]).is_err());
}

fn config_in(dir: &std::path::Path, text: &str) -> ResetArgs {
    let file = dir.join("serve.toml");
    std::fs::write(&file, text).unwrap();
    let mut flags = Flags::new();
    flags.insert("config", Value::Path(file));
    ResetArgs {
        name: "owner".to_owned(),
        flags,
    }
}

#[test]
fn a_reset_prints_a_link_on_the_public_address_whose_secret_the_server_redeems() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    let args = config_in(
        dir.path(),
        "data_dir = \".\"\nremote = true\npublic_hosts = [\"puddle.example.org\"]\n",
    );
    let link = execute(&args).unwrap();
    let secret = link
        .strip_prefix("https://puddle.example.org/invite#")
        .unwrap();
    assert_eq!(secret.len(), 43);
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    let digest: [u8; 32] = sha2::Sha256::digest(secret.as_bytes()).into();
    assert!(
        store
            .take_account_secrets()
            .unwrap()
            .redeem_invite(&digest)
            .unwrap()
            .is_some()
    );
}

#[test]
fn without_a_public_address_nothing_is_reset() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    let args = config_in(dir.path(), "data_dir = \".\"\n");
    let err = execute(&args).unwrap_err();
    assert!(err.contains("no public address"), "{err}");
    // The owner's password and devices were not touched: there is no invite either.
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    let secrets = store.take_account_secrets().unwrap();
    assert!(!secrets.has_password(puddle_store::UserId::OWNER).unwrap());
}

#[test]
fn a_user_that_does_not_exist_or_a_missing_folder_is_said_so() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    let mut args = config_in(
        dir.path(),
        "data_dir = \".\"\nremote = true\npublic_hosts = [\"puddle.example.org\"]\n",
    );
    args.name = "ghost".to_owned();
    assert_eq!(execute(&args).unwrap_err(), "no user named ghost");
    let empty = tempfile::tempdir().unwrap();
    let args = config_in(
        empty.path(),
        "data_dir = \".\"\nremote = true\npublic_hosts = [\"puddle.example.org\"]\n",
    );
    assert!(execute(&args).unwrap_err().contains("no database"));
}

#[test]
fn a_config_file_that_is_wrong_stops_the_reset_with_its_message() {
    let dir = tempfile::tempdir().unwrap();
    let args = config_in(dir.path(), "prot = 1\n");
    assert!(execute(&args).unwrap_err().contains("unknown key `prot`"));
}

struct Broken;

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_link_is_written_on_its_own_line_and_a_failed_write_fails_the_command() {
    let mut out = Vec::new();
    assert_eq!(
        print_link(&mut out, "https://x/invite#s"),
        ExitCode::SUCCESS
    );
    assert_eq!(out, b"https://x/invite#s\n");
    assert_eq!(
        print_link(&mut Broken, "https://x/invite#s"),
        ExitCode::FAILURE
    );
}
