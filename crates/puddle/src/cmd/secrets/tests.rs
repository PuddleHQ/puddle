// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{KeySourceTag, StoreRecord};

use super::*;
use crate::cmd::serve::options::Value;

fn parsed(args: &[&str]) -> Result<VaultAdminArgs, UsageError> {
    match parse(args.iter()) {
        Ok(Command::VaultAdmin(a)) => Ok(a),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

#[test]
fn the_three_commands_parse_with_the_options_that_find_the_data_folder() {
    assert_eq!(parsed(&["status"]).unwrap().action, Action::Status);
    assert_eq!(
        parsed(&["migrate", "--to", "file"]).unwrap().action,
        Action::Migrate(StoreKind::File)
    );
    assert_eq!(
        parsed(&["migrate", "--to", "os"]).unwrap().action,
        Action::Migrate(StoreKind::Os)
    );
    assert_eq!(
        parsed(&["reset"]).unwrap().action,
        Action::Reset { yes: false }
    );
    let a = parsed(&["reset", "--yes", "--config", "s.toml", "--data-dir", "d"]).unwrap();
    assert_eq!(a.action, Action::Reset { yes: true });
    assert_eq!(a.flags.get("config"), Some(&Value::Path("s.toml".into())));
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
    let a = parsed(&["status", "--data-dir", "d"]).unwrap();
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
}

#[test]
fn anything_else_is_a_usage_error() {
    assert_eq!(parsed(&[]), Err(UsageError::MissingArgument(USAGE_ANY)));
    assert_eq!(
        parsed(&["migrate"]),
        Err(UsageError::MissingArgument(USAGE_MIGRATE))
    );
    assert_eq!(
        parsed(&["migrate", "--to"]),
        Err(UsageError::MissingArgument("--to os|file"))
    );
    assert_eq!(
        parsed(&["migrate", "--to", "keyring"]),
        Err(UsageError::InvalidValue {
            option: "--to",
            expected: "os or file",
            value: "keyring".into()
        })
    );
    assert_eq!(
        parsed(&["delete"]),
        Err(UsageError::UnknownArgument("delete".into()))
    );
    assert!(parsed(&["status", "--yes"]).is_err());
    assert!(parsed(&["status", "--to", "os"]).is_err());
    assert!(parsed(&["migrate", "--to", "os", "--yes"]).is_err());
    assert!(parsed(&["migrate", "--to", "os", "--to", "file"]).is_err());
    assert!(parsed(&["reset", "--yes", "--yes"]).is_err());
    assert!(parsed(&["reset", "now"]).is_err());
    assert!(parsed(&["status", "--data-dir", "a", "--data-dir", "b"]).is_err());
}

fn record(store: StoreKind) -> StoreRecord {
    StoreRecord {
        version: 1,
        store,
        decided: "2026-11-16T05:33:55Z".into(),
        why: "Linux, started with the headless flag".into(),
    }
}

#[test]
fn status_names_the_store_when_it_was_chosen_the_key_source_and_the_counts() {
    let text = status_text(&VaultStatus {
        record: Some(record(StoreKind::File)),
        named: 3,
        key_source: Some(KeySourceTag::KeyFile),
        state: StoreState::Readable { found: 2 },
    });
    assert_eq!(
        text,
        "store: Puddle's file vault\n\
         chosen: 2026-11-16T05:33:55Z (Linux, started with the headless flag)\n\
         key source: key-file\n\
         named by rows: 3 secrets\n\
         state: readable, 2 of the named secrets found"
    );
    let os = status_text(&VaultStatus {
        record: Some(record(StoreKind::Os)),
        named: 1,
        key_source: None,
        state: StoreState::Readable { found: 1 },
    });
    assert!(
        os.starts_with("store: the operating system's credential store\n"),
        "{os}"
    );
    assert!(os.contains("named by rows: 1 secret\n"), "{os}");
    assert!(!os.contains("key source"), "{os}");
}

#[test]
fn status_gives_the_reason_a_store_cannot_be_read_and_the_other_states() {
    let none = status_text(&VaultStatus {
        record: None,
        named: 0,
        key_source: None,
        state: StoreState::Undecided,
    });
    assert!(none.starts_with("store: not chosen yet"), "{none}");
    assert!(!none.contains("state:"), "{none}");
    let no_vault = status_text(&VaultStatus {
        record: Some(record(StoreKind::File)),
        named: 0,
        key_source: None,
        state: StoreState::NoVault,
    });
    assert!(
        no_vault.contains("state: there is no vault file yet"),
        "{no_vault}"
    );
    let bad = status_text(&VaultStatus {
        record: Some(record(StoreKind::File)),
        named: 2,
        key_source: Some(KeySourceTag::SystemdHost),
        state: StoreState::Unreadable {
            title: "its key is missing".into(),
            fix: "restore it".into(),
        },
    });
    assert!(
        bad.contains("state: cannot be read: its key is missing\nfix: restore it"),
        "{bad}"
    );
    assert!(bad.contains("key source: systemd-host"), "{bad}");
}

#[test]
fn the_commands_word_counts_and_never_a_value_or_an_id() {
    // The result types hold counts and problem texts only, so a canary cannot reach these texts;
    // this fixes what each one says and that none carries more than a count.
    let moved = migrate_text(
        MigrateOutcome::Moved { copied: 3, left: 0 },
        StoreKind::File,
    );
    assert_eq!(
        moved,
        "Moved 3 values to Puddle's file vault. Start Puddle again to use it."
    );
    let one = migrate_text(MigrateOutcome::Moved { copied: 1, left: 1 }, StoreKind::Os);
    assert_eq!(
        one,
        "Moved 1 value to the operating system's credential store. Start Puddle again to use it.\n1 value could not be removed from the old store; run this command again to remove it."
    );
    let many = migrate_text(MigrateOutcome::Moved { copied: 0, left: 2 }, StoreKind::Os);
    assert!(many.contains("2 values could not be removed"), "{many}");
    assert!(many.contains("remove them"), "{many}");
    let finished = migrate_text(
        MigrateOutcome::Finished {
            removed: 2,
            left: 1,
        },
        StoreKind::File,
    );
    assert_eq!(
        finished,
        "An earlier move to Puddle's file vault had stopped before it finished; removed 2 leftover values from the old store.\n1 value could not be removed; run this command again."
    );
    let recorded = migrate_text(MigrateOutcome::Recorded, StoreKind::Os);
    assert!(recorded.contains("no values moved"), "{recorded}");
    let there = migrate_text(
        MigrateOutcome::AlreadyThere(StoreKind::File),
        StoreKind::File,
    );
    assert!(there.contains("nothing to move"), "{there}");
    for text in [&moved, &one, &many, &finished, &recorded, &there] {
        assert!(!text.contains("env-") && !text.contains("proxy-"), "{text}");
    }
}

#[test]
fn reset_says_what_it_would_do_and_what_it_cost() {
    let plan = reset_text(ResetOutcome::Plan { key_files: 1 });
    assert!(plan.starts_with("Nothing was changed."), "{plan}");
    assert!(plan.contains("1 key file in your config folder"), "{plan}");
    assert!(
        plan.contains("proxy sign-ins lived in the vault and are gone"),
        "{plan}"
    );
    assert!(
        plan.ends_with("Run `puddle secrets reset --yes` to do it."),
        "{plan}"
    );
    let done = reset_text(ResetOutcome::Done { key_files: 0 });
    assert!(done.starts_with("Set the old vault aside"), "{done}");
    assert!(done.contains("0 key files likewise"), "{done}");
    assert!(!done.contains("Nothing was changed"), "{done}");
    assert!(done.contains("lists each as missing"), "{done}");
}

fn args_for(action: Action, data: &std::path::Path) -> VaultAdminArgs {
    let mut flags = Flags::new();
    flags.insert("data_dir", Value::Path(data.to_path_buf()));
    VaultAdminArgs { action, flags }
}

fn exit_is_success(code: ExitCode) -> bool {
    format!("{code:?}") == format!("{:?}", ExitCode::SUCCESS)
}

#[test]
fn migrating_a_folder_with_no_record_to_os_only_records_it() {
    let dir = tempfile::tempdir().unwrap();
    let args = args_for(Action::Migrate(StoreKind::Os), dir.path());
    let report = execute_with(&args, &SecretSetup::default()).unwrap();
    assert!(report.ok);
    assert!(report.text.contains("no values moved"), "{}", report.text);
    assert!(exit_is_success(run(&args)));
}

#[test]
fn status_of_a_vault_that_is_not_one_fails_the_command_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    puddle_secrets::write_record(dir.path(), StoreKind::File, puddle_secrets::Rule::Headless)
        .unwrap();
    std::fs::write(puddle_secrets::vault_path(dir.path()), b"just some text").unwrap();
    let args = args_for(Action::Status, dir.path());
    let report = execute_with(&args, &SecretSetup::default()).unwrap();
    assert!(!report.ok);
    assert!(
        report.text.contains("state: cannot be read"),
        "{}",
        report.text
    );
    assert!(!exit_is_success(run(&args)));
}

#[test]
fn a_command_the_folder_refuses_fails_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let _host = puddle_fs::DataLock::acquire(dir.path()).unwrap();
    let args = args_for(Action::Migrate(StoreKind::File), dir.path());
    let err = execute_with(&args, &SecretSetup::default())
        .unwrap_err()
        .to_string();
    assert!(err.contains("already using the data folder"), "{err}");
    assert!(!exit_is_success(run(&args)));
}

#[cfg(target_os = "linux")]
#[test]
fn reset_says_what_it_would_do_for_a_vault_whose_key_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    puddle_secrets::write_record(&data, StoreKind::File, puddle_secrets::Rule::Headless).unwrap();
    puddle_secrets::FileVault::create(
        &puddle_secrets::vault_path(&data),
        puddle_secrets::VaultKey::from_bytes([3; 32]),
        KeySourceTag::KeyFile,
    )
    .unwrap();
    let mut setup = SecretSetup::default();
    setup.key_home = Some(dir.path().join("config"));
    setup.tools = Some(puddle_secrets::ToolPaths::default());
    let report = execute_with(&args_for(Action::Reset { yes: false }, &data), &setup).unwrap();
    assert!(report.ok);
    assert!(
        report.text.starts_with("Nothing was changed."),
        "{}",
        report.text
    );
    assert!(puddle_secrets::vault_path(&data).is_file(), "nothing moved");
}
