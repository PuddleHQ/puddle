// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::workspaces::{DisplayName, Listing, Operation};

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
fn each_operation_is_admitted_in_the_states_it_needs_and_refused_in_the_rest() {
    use WorkspaceStatus::{Crashed, Created, Draining, Running, Starting, Stopped};
    for status in [Created, Stopped, Crashed] {
        let r = record(status);
        assert!(admit_start(&r).is_ok());
        assert!(admit_reset_root_disk(&r).is_ok());
        assert!(admit_delete_status(&r).is_ok());
        says(&admit_stop(&r), "is not running");
    }
    for status in [Running, Starting, Draining] {
        let r = record(status);
        says(&admit_start(&r), "is already");
        says(&admit_reset_root_disk(&r), "stop it before resetting");
        says(&admit_delete_status(&r), "stop it before deleting");
    }
    assert!(admit_stop(&record(Running)).is_ok());
    says(&admit_settled(&record(Starting)), "try again in a moment");
    says(&admit_settled(&record(Draining)), "try again in a moment");
    assert!(admit_settled(&record(Running)).is_ok());
}

#[test]
fn a_busy_workspace_takes_no_second_operation() {
    let mut r = record(WorkspaceStatus::Running);
    assert!(admit_idle(&r).is_ok());
    r.busy = Some(Operation::Stopping);
    says(&admit_idle(&r), "is busy (stopping)");
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
