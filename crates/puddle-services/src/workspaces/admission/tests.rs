// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::workspaces::{DisplayName, Listing};
use puddle_types::WorkspaceStatus;

fn record(status: WorkspaceStatus) -> WorkspaceRecord {
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        puddle_store::UserId::OWNER,
        DisplayName::parse("web").unwrap(),
        Vec::new(),
    );
    record.status = status;
    record
}

fn says(result: &Result<(), WorkspaceError>, words: &str) {
    let shown = format!("{result:?}");
    assert!(
        shown.starts_with("Err(") && shown.contains(words),
        "expected a refusal saying {words:?}, got {shown}"
    );
}

#[test]
fn a_root_disk_only_grows() {
    assert!(admit_grow_size(5000, 4096).is_ok());
    says(&admit_grow_size(4096, 4096), "only grows");
    says(&admit_grow_size(100, 4096), "its 4096 MiB");
}

#[test]
fn delete_needs_the_fingerprint_the_check_gave_or_a_clean_check() {
    let r = record(WorkspaceStatus::Stopped);
    let clean = DeleteCheck::new(
        r.id.clone(),
        Vec::new(),
        Listing::default(),
        Vec::new(),
        None,
    );
    assert!(admit_delete_fingerprint(&r, None, &clean).is_ok());
    assert!(admit_delete_fingerprint(&r, Some(&clean.fingerprint), &clean).is_ok());
    says(
        &admit_delete_fingerprint(&r, Some("stale"), &clean),
        "has changed since it was checked",
    );
}

#[test]
fn an_unknown_id_is_not_found_and_names_the_id() {
    let id = WorkspaceId::new("ghost").unwrap();
    assert!(matches!(not_found(&id), WorkspaceError::NotFound(m) if m == "no workspace ghost"));
}
