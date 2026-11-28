// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_host::HostPaths;
use puddle_store::{Limits, ManualClock, Store};

use super::*;
use crate::cmd::serve::options::Value;

fn audit(args: &[&str]) -> Result<AuditArgs, UsageError> {
    match parse(args.iter()) {
        Ok(Command::Audit(a)) => Ok(a),
        Ok(other) => panic!("{other:?}"),
        Err(e) => Err(e),
    }
}

#[test]
fn status_and_prune_parse_with_their_options() {
    assert_eq!(audit(&["status"]).unwrap().action, Action::Status);
    assert_eq!(audit(&["prune"]).unwrap().action, Action::Prune(None));
    let a = audit(&["prune", "--days", "30", "--data-dir", "d"]).unwrap();
    assert_eq!(a.action, Action::Prune(Some(30)));
    assert_eq!(a.flags.get("data_dir"), Some(&Value::Path("d".into())));
}

#[test]
fn anything_else_is_a_usage_error() {
    assert_eq!(
        audit(&[]),
        Err(UsageError::MissingArgument(
            "audit status|prune [--days <n>]"
        ))
    );
    assert_eq!(
        audit(&["clear"]),
        Err(UsageError::UnknownArgument("clear".into()))
    );
    assert!(audit(&["status", "--days", "3"]).is_err());
    assert!(audit(&["prune", "--days", "3", "--days", "4"]).is_err());
    assert!(audit(&["prune", "extra"]).is_err());
    assert_eq!(
        audit(&["prune", "--days"]),
        Err(UsageError::MissingArgument("--days <n>"))
    );
    assert!(matches!(
        audit(&["prune", "--days", "many"]),
        Err(UsageError::InvalidValue { .. })
    ));
}

const DAY: u64 = 24 * 60 * 60 * 1000;

fn args_in(dir: &std::path::Path, action: Action) -> AuditArgs {
    let file = dir.join("serve.toml");
    std::fs::write(&file, "data_dir = \".\"\n").unwrap();
    let mut flags = Flags::new();
    flags.insert("config", Value::Path(file));
    AuditArgs { action, flags }
}

fn lines(paths: &HostPaths) -> Vec<String> {
    let store = Store::open(
        &paths.store(),
        Arc::new(ManualClock::new(1)),
        Limits::default(),
    )
    .unwrap();
    store
        .audit_lines(0, 1000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| line)
        .filter(|line| line.contains(".example"))
        .collect()
}

#[test]
fn prune_deletes_only_records_older_than_the_days_and_status_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    let now = puddle_store::Clock::now_ms(&puddle_store::SystemClock);
    // A request 40 days old and one 10 days old, as the clock of the server that wrote them saw it.
    let clock = Arc::new(ManualClock::new(now - 40 * DAY));
    let store = Store::open(&paths.store(), clock.clone(), Limits::default()).unwrap();
    let workspace = puddle_types::WorkspaceId::new("a").unwrap();
    puddle_store::testing::own(&store, &workspace);
    for (age, host) in [(40, "forty.example"), (10, "ten.example")] {
        clock.set(now - age * DAY);
        let host = puddle_types::Host::parse_normalised(host).unwrap();
        let request = puddle_types::EgressRequest::new(workspace.clone(), host, 443);
        store
            .decide(&request, puddle_types::SuffixAllows::Count)
            .unwrap();
    }
    store.set_audit_retention_days(60).unwrap();
    drop(store);
    let status = execute(&args_in(dir.path(), Action::Status)).unwrap();
    assert!(status.contains("kept for 60 days"), "{status}");
    let done = execute(&args_in(dir.path(), Action::Prune(Some(30)))).unwrap();
    assert!(done.contains("older than 30 days"), "{done}");
    let left = lines(&paths);
    assert!(left.iter().all(|l| l.contains("ten.example")), "{left:?}");
    assert_eq!(left.len(), 1, "{left:?}");
    // Without --days the saved retention (60 days) applies, and nothing is that old.
    let done = execute(&args_in(dir.path(), Action::Prune(None))).unwrap();
    assert!(
        done.contains("deleted 0 records older than 60 days"),
        "{done}"
    );
    let err = execute(&args_in(dir.path(), Action::Prune(Some(0))))
        .unwrap_err()
        .to_string();
    assert!(err.contains("1 to 3650"), "{err}");
}

#[test]
fn a_missing_data_folder_is_said_so() {
    let empty = tempfile::tempdir().unwrap();
    let err = execute(&args_in(empty.path(), Action::Status))
        .unwrap_err()
        .to_string();
    assert!(err.contains("no database"), "{err}");
}

#[test]
fn sizes_read_in_tenths_of_a_mebibyte() {
    assert_eq!(mib(0), "0.0 MiB");
    assert_eq!(mib(1_572_864), "1.5 MiB");
    assert_eq!(mib(10 * 1024 * 1024 + 104_858), "10.1 MiB");
}

fn stats(file_in_use: u64) -> puddle_host::AuditStats {
    puddle_host::AuditStats {
        records: 12,
        bytes: 2_097_152,
        oldest_ts: Some(1),
        records_last_day: 3,
        bytes_last_day: 1_048_576,
        records_last_week: 9,
        bytes_last_week: 2_097_152,
        retention_days: 90,
        warn_bytes: 2_097_152,
        file_bytes: file_in_use + 524_288,
        free_bytes: 524_288,
    }
}

#[test]
fn the_status_warns_only_past_the_warning_size() {
    let at = status_text(&stats(2_097_152));
    assert!(
        at.starts_with("kept for 90 days\n12 records, 2.0 MiB of text"),
        "{at}"
    );
    assert!(
        at.contains(
            "written in the last day: 3 records, 1.0 MiB; in the last week: 9 records, 2.0 MiB"
        ),
        "{at}"
    );
    assert!(!at.contains("over the"), "{at}");
    let over = status_text(&stats(2_097_153));
    assert!(
        over.ends_with("shorten the days or run `audit prune`."),
        "{over}"
    );
    assert!(
        over.contains("over the 2.0 MiB where Puddle warns"),
        "{over}"
    );
}
