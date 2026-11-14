// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock};
use sha2::Digest as _;

use super::*;
use crate::cmd::serve::options::Value;

fn user(args: &[&str]) -> Result<UserArgs, UsageError> {
    match parse(args.iter()) {
        Ok(Command::User(a)) => Ok(a),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

#[test]
fn reset_takes_a_name_and_the_two_options_that_find_the_server() {
    let a = user(&["reset", "ann"]).unwrap();
    assert_eq!(a.action, Action::Reset("ann".into()));
    assert!(a.flags.is_empty());
    let a = user(&["reset", "--config", "s.toml", "ann", "--data-dir", "d"]).unwrap();
    assert_eq!(a.action, Action::Reset("ann".into()));
    assert_eq!(a.flags.get("config"), Some(&Value::Path("s.toml".into())));
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
}

#[test]
fn anything_else_is_a_usage_error() {
    assert_eq!(
        user(&[]),
        Err(UsageError::MissingArgument("user add|list|remove|reset"))
    );
    assert_eq!(
        user(&["reset"]),
        Err(UsageError::MissingArgument("user reset <name>"))
    );
    assert_eq!(
        user(&["delete", "ann"]),
        Err(UsageError::UnknownArgument("delete".into()))
    );
    assert!(user(&["reset", "ann", "bob"]).is_err());
    assert!(user(&["reset", "ann", "--port", "1"]).is_err());
    assert!(user(&["reset", "ann", "--password", "x"]).is_err());
    assert!(user(&["reset", "--config", "a", "--config", "b", "ann"]).is_err());
    assert_eq!(
        user(&["reset", "ann", "--config"]),
        Err(UsageError::MissingArgument("--config <path>"))
    );
    assert!(user(&["reset", "ann", "--data-dir", ""]).is_err());
}

fn config_in(dir: &std::path::Path, text: &str) -> UserArgs {
    config_with(dir, text, Action::Reset("owner".to_owned()))
}

fn config_with(dir: &std::path::Path, text: &str, action: Action) -> UserArgs {
    let file = dir.join("serve.toml");
    std::fs::write(&file, text).unwrap();
    let mut flags = Flags::new();
    flags.insert("config", Value::Path(file));
    UserArgs { action, flags }
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
    let link = execute(&args).unwrap().result;
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
    args.action = Action::Reset("ghost".to_owned());
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
        write_result(&mut out, "https://x/invite#s"),
        ExitCode::SUCCESS
    );
    assert_eq!(out, b"https://x/invite#s\n");
    assert_eq!(
        write_result(&mut Broken, "https://x/invite#s"),
        ExitCode::FAILURE
    );
    assert_eq!(
        write_result(&mut Broken, ""),
        ExitCode::SUCCESS,
        "nothing to write"
    );
}

const REMOTE: &str = "data_dir = \".\"\nremote = true\npublic_hosts = [\"puddle.example.org\"]\n";

fn with_database() -> (tempfile::TempDir, HostPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    (dir, paths)
}

#[test]
fn add_list_and_remove_take_what_they_need_and_nothing_else() {
    let a = user(&["add", "--data-dir", "d", "ann"]).unwrap();
    assert_eq!(a.action, Action::Add("ann".into()));
    assert_eq!(
        user(&["remove", "ann"]).unwrap().action,
        Action::Remove("ann".into())
    );
    let a = user(&["list", "--config", "s.toml"]).unwrap();
    assert_eq!(a.action, Action::List);
    assert_eq!(a.flags.get("config"), Some(&Value::Path("s.toml".into())));
    assert_eq!(
        user(&["add"]),
        Err(UsageError::MissingArgument("user add <name>"))
    );
    assert_eq!(
        user(&["remove"]),
        Err(UsageError::MissingArgument("user remove <name>"))
    );
    assert!(user(&["list", "ann"]).is_err());
    assert!(user(&["list", "--password", "x"]).is_err());
    assert!(user(&["add", "ann", "bob"]).is_err());
}

#[test]
fn an_add_prints_a_link_the_server_redeems_and_a_list_and_remove_follow_it() {
    let (dir, paths) = with_database();
    let add = config_with(dir.path(), REMOTE, Action::Add("Ann".into()));
    let out = execute(&add).unwrap();
    assert!(out.notice.contains("Ann is added"), "{out:?}");
    let secret = out
        .result
        .strip_prefix("https://puddle.example.org/invite#")
        .unwrap();
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    let digest: [u8; 32] = sha2::Sha256::digest(secret.as_bytes()).into();
    let ann = store.user_named("Ann").unwrap().unwrap().id;
    assert_eq!(
        store
            .take_account_secrets()
            .unwrap()
            .redeem_invite(&digest)
            .unwrap(),
        Some(ann)
    );
    drop(store);

    let list = execute(&config_with(dir.path(), REMOTE, Action::List)).unwrap();
    assert_eq!(list.notice, "");
    assert!(list.result.starts_with("name"));
    assert!(list.result.contains("Ann"));
    assert!(list.result.contains("invited"));
    assert!(!list.result.contains(secret));

    let removed = execute(&config_with(
        dir.path(),
        REMOTE,
        Action::Remove("Ann".into()),
    ))
    .unwrap();
    assert_eq!(removed.result, "");
    assert!(removed.notice.contains("cannot be undone"), "{removed:?}");
    let list = execute(&config_with(dir.path(), REMOTE, Action::List)).unwrap();
    assert!(list.result.contains("deleting"));
}

#[test]
fn an_add_without_a_public_address_makes_no_user_and_a_remove_needs_none() {
    let (dir, paths) = with_database();
    let local = "data_dir = \".\"\n";
    let err = execute(&config_with(dir.path(), local, Action::Add("Ann".into()))).unwrap_err();
    assert!(err.contains("no public address"), "{err}");
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    assert!(store.user_named("Ann").unwrap().is_none());
    drop(store);
    let err = execute(&config_with(
        dir.path(),
        local,
        Action::Remove("ghost".into()),
    ))
    .unwrap_err();
    assert_eq!(err, "no user named ghost");
    let err = execute(&config_with(
        dir.path(),
        local,
        Action::Remove("owner".into()),
    ))
    .unwrap_err();
    assert!(err.contains("can't be deleted"), "{err}");
}

#[test]
fn the_last_sign_in_reads_as_how_long_ago() {
    assert_eq!(ago(1_000_000, None), "never");
    assert_eq!(ago(1_000_000, Some(990_000)), "just now");
    assert_eq!(ago(1_000_000, Some(1_000_000 - 120_000)), "2 min ago");
    assert_eq!(ago(10_000_000, Some(10_000_000 - 7_300_000)), "2 h ago");
    assert_eq!(ago(900_000_000, Some(900_000_000 - 200_000_000)), "2 d ago");
    assert_eq!(ago(5, Some(10)), "just now");
}

#[test]
fn a_user_who_chose_a_password_shows_as_active_in_the_list() {
    let (dir, paths) = with_database();
    execute(&config_with(dir.path(), REMOTE, Action::Add("Ann".into()))).unwrap();
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    let ann = store.user_named("Ann").unwrap().unwrap().id;
    store
        .take_account_secrets()
        .unwrap()
        .set_password_hash(ann, "a stored hash")
        .unwrap();
    drop(store);
    let list = execute(&config_with(dir.path(), REMOTE, Action::List)).unwrap();
    let ann_line = list.result.lines().find(|l| l.starts_with("Ann")).unwrap();
    assert!(ann_line.contains("active"));
}
