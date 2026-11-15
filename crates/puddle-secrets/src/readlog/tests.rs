// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::*;
use crate::error::Tool;
use crate::name::{AccountName, HostName};

fn gh(account: &str) -> SourceSpec {
    SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: AccountName::new(account).unwrap(),
    }
}

fn counted() -> (ReadLog, Arc<AtomicUsize>) {
    let moves = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&moves);
    let log = ReadLog::new().with_on_change(move || {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    (log, moves)
}

#[test]
fn the_log_describes_itself_without_its_entries() {
    let log = ReadLog::new();
    log.record(&gh("me"), ReadUse::Test, None);
    assert_eq!(format!("{log:?}"), "ReadLog { .. }");
}

#[test]
fn a_source_has_no_entry_before_its_first_read() {
    assert_eq!(ReadLog::new().last(&gh("me")), None);
}

#[test]
fn the_last_read_wins_and_keeps_its_time_and_use() {
    let log = ReadLog::new();
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
    log.record_at(
        &gh("me"),
        ReadUse::Injection,
        Some(SourceError::NotSignedIn),
        t,
    );
    log.record_at(&gh("me"), ReadUse::Test, None, t + Duration::from_secs(5));
    assert_eq!(
        log.last(&gh("me")),
        Some(LastRead {
            at: t + Duration::from_secs(5),
            by: ReadUse::Test,
            problem: None
        })
    );
    assert_eq!(log.last(&gh("other")), None);
}

#[test]
fn only_a_moved_outcome_tells_the_screens() {
    let (log, moves) = counted();
    log.record(&gh("me"), ReadUse::Injection, None);
    log.record(&gh("me"), ReadUse::RepositoryList, None);
    assert_eq!(moves.load(Ordering::SeqCst), 1, "first read, then a repeat");
    log.record(
        &gh("me"),
        ReadUse::Injection,
        Some(SourceError::NotSignedIn),
    );
    assert_eq!(moves.load(Ordering::SeqCst), 2, "readable to a problem");
    log.record(
        &gh("me"),
        ReadUse::Injection,
        Some(SourceError::NotSignedIn),
    );
    assert_eq!(moves.load(Ordering::SeqCst), 2, "the same problem again");
    log.record(
        &gh("me"),
        ReadUse::Test,
        Some(SourceError::ToolMissing(Tool::Gh)),
    );
    assert_eq!(moves.load(Ordering::SeqCst), 3, "another problem");
}

#[test]
fn a_flood_of_sources_stops_at_the_cap_without_dropping_known_ones() {
    let log = ReadLog::new();
    for n in 0..MAX_SOURCES {
        log.record(&gh(&format!("a{n}")), ReadUse::Injection, None);
    }
    log.record(&gh("late"), ReadUse::Injection, None);
    assert_eq!(log.last(&gh("late")), None);
    log.record(&gh("a0"), ReadUse::Test, Some(SourceError::NotSignedIn));
    assert_eq!(log.last(&gh("a0")).unwrap().by, ReadUse::Test);
}
