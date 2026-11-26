// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::workspaces::DisplayName;
use puddle_types::WorkspaceId;

use Operation::{
    Cloning, Creating, Deleting, GrowingRootDisk, Reclaiming, ResettingRootDisk, Starting, Stopping,
};
use WorkspaceStatus as S;

const STATUSES: [WorkspaceStatus; 8] = [
    S::Created,
    S::Starting,
    S::Running,
    S::Draining,
    S::Paused,
    S::Stopped,
    S::Crashed,
    S::VolumeMissing,
];

const OPERATIONS: [Operation; 8] = [
    Creating,
    Starting,
    Stopping,
    Reclaiming,
    ResettingRootDisk,
    GrowingRootDisk,
    Cloning,
    Deleting,
];

fn record(status: WorkspaceStatus, busy: Option<Operation>) -> WorkspaceRecord {
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        puddle_store::UserId::OWNER,
        DisplayName::parse("web").unwrap(),
        Vec::new(),
    );
    record.status = status;
    record.busy = busy;
    record
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the tables compare with what `transition` returns"
)]
fn idle(status: WorkspaceStatus) -> Result<Next, Refused> {
    Ok(Next::Phase(Phase::Idle(status)))
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the tables compare with what `transition` returns"
)]
fn busy(op: Operation, status: WorkspaceStatus) -> Result<Next, Refused> {
    Ok(Next::Phase(Phase::Busy(op, status)))
}

fn says(result: &Result<(), WorkspaceError>, words: &str) {
    let shown = format!("{result:?}");
    assert!(
        shown.starts_with("Err(Conflict(") && shown.contains(words),
        "expected a conflict saying {words:?}, got {shown}"
    );
}

#[test]
fn each_operation_begins_only_in_the_statuses_of_its_row() {
    // op, the statuses an idle workspace begins it in, the status it shows while it runs
    // (`None`: the one it had).
    let table: [(Operation, &[WorkspaceStatus], Option<WorkspaceStatus>); 8] = [
        (Creating, &[S::Created], None),
        (
            Starting,
            &[S::Created, S::Stopped, S::Crashed, S::VolumeMissing],
            Some(S::Starting),
        ),
        (Stopping, &[S::Running], Some(S::Draining)),
        (
            Reclaiming,
            &[S::Created, S::Running, S::Paused, S::Stopped, S::Crashed],
            None,
        ),
        (
            ResettingRootDisk,
            &[S::Created, S::Stopped, S::Crashed, S::VolumeMissing],
            None,
        ),
        (
            GrowingRootDisk,
            &[
                S::Created,
                S::Running,
                S::Paused,
                S::Stopped,
                S::Crashed,
                S::VolumeMissing,
            ],
            None,
        ),
        (Cloning, &[S::Running], None),
        (
            Deleting,
            &[S::Created, S::Stopped, S::Crashed, S::VolumeMissing],
            None,
        ),
    ];
    for (op, begins_in, shows) in table {
        for status in STATUSES {
            let got = transition(Phase::Idle(status), Move::Begin(op));
            let want = if begins_in.contains(&status) {
                busy(op, shows.unwrap_or(status))
            } else {
                Err(Refused::Status(status))
            };
            assert_eq!(got, want, "{op} from idle {status}");
        }
    }
}

#[test]
fn a_busy_workspace_begins_nothing_else() {
    for running in OPERATIONS {
        for status in STATUSES {
            for op in OPERATIONS {
                assert_eq!(
                    transition(Phase::Busy(running, status), Move::Begin(op)),
                    Err(Refused::Busy(running)),
                    "{op} while {running} ({status})"
                );
            }
        }
    }
}

#[test]
fn each_end_leaves_the_workspace_where_its_row_says() {
    use Outcome::{Done, Failed, VolumeGone};
    let removed = Ok(Next::Removed);
    // op, the status shown when the work ended, the outcome, where it leads
    let table = [
        (Creating, S::Created, Done, idle(S::Running)),
        (Creating, S::Created, Failed, removed),
        (Creating, S::Created, VolumeGone, removed),
        (Starting, S::Starting, Done, idle(S::Running)),
        (Starting, S::Starting, Failed, idle(S::Crashed)),
        (Starting, S::Starting, VolumeGone, idle(S::VolumeMissing)),
        (Stopping, S::Draining, Done, idle(S::Stopped)),
        (Stopping, S::Draining, Failed, idle(S::Running)),
        (Deleting, S::Stopped, Done, removed),
        (Deleting, S::Stopped, Failed, idle(S::Stopped)),
        (Reclaiming, S::Running, Done, idle(S::Running)),
        (Reclaiming, S::Stopped, Failed, idle(S::Stopped)),
        (ResettingRootDisk, S::Crashed, Done, idle(S::Crashed)),
        (ResettingRootDisk, S::Stopped, Failed, idle(S::Stopped)),
        (Cloning, S::Running, Done, idle(S::Running)),
        (Cloning, S::Running, Failed, idle(S::Running)),
        // A grow that needed no restart.
        (GrowingRootDisk, S::Running, Done, idle(S::Running)),
        (GrowingRootDisk, S::Stopped, Done, idle(S::Stopped)),
        (GrowingRootDisk, S::Stopped, Failed, idle(S::Stopped)),
        // A grow whose restart came back, failed in its start, or failed in its stop.
        (GrowingRootDisk, S::Starting, Done, idle(S::Running)),
        (GrowingRootDisk, S::Starting, Failed, idle(S::Crashed)),
        (
            GrowingRootDisk,
            S::Starting,
            VolumeGone,
            idle(S::VolumeMissing),
        ),
        (GrowingRootDisk, S::Draining, Failed, idle(S::Running)),
        // The monitor saw the VM die while a reclaim ran: the end keeps that.
        (Reclaiming, S::Crashed, Done, idle(S::Crashed)),
    ];
    for (op, status, outcome, want) in table {
        assert_eq!(
            transition(Phase::Busy(op, status), Move::End(outcome)),
            want,
            "{op} ending {outcome:?} while {status}"
        );
    }
}

#[test]
fn the_other_events_move_only_the_phases_of_their_row() {
    let table = [
        // A request dropped before its work began: a create leaves nothing, the rest go idle
        // with the status as it is.
        (
            Phase::Busy(Creating, S::Created),
            Move::Withdraw,
            Ok(Next::Removed),
        ),
        (
            Phase::Busy(Deleting, S::Stopped),
            Move::Withdraw,
            idle(S::Stopped),
        ),
        (
            Phase::Busy(Reclaiming, S::Crashed),
            Move::Withdraw,
            idle(S::Crashed),
        ),
        (
            Phase::Idle(S::Stopped),
            Move::Withdraw,
            Err(Refused::NoSuchOperation),
        ),
        // A grow's restart, and nothing else, goes down and up.
        (
            Phase::Busy(GrowingRootDisk, S::Running),
            Move::RestartStops,
            busy(GrowingRootDisk, S::Draining),
        ),
        (
            Phase::Busy(GrowingRootDisk, S::Draining),
            Move::RestartStarts,
            busy(GrowingRootDisk, S::Starting),
        ),
        (
            Phase::Busy(GrowingRootDisk, S::Stopped),
            Move::RestartStops,
            Err(Refused::NoSuchOperation),
        ),
        (
            Phase::Busy(Reclaiming, S::Running),
            Move::RestartStops,
            Err(Refused::NoSuchOperation),
        ),
        (
            Phase::Idle(S::Running),
            Move::RestartStarts,
            Err(Refused::NoSuchOperation),
        ),
        // A dead VM crashes an idle running workspace; an operation in progress decides itself.
        (Phase::Idle(S::Running), Move::VmDied, idle(S::Crashed)),
        (Phase::Idle(S::Stopped), Move::VmDied, idle(S::Stopped)),
        (
            Phase::Busy(Reclaiming, S::Running),
            Move::VmDied,
            busy(Reclaiming, S::Running),
        ),
        (
            Phase::Idle(S::Running),
            Move::End(Outcome::Done),
            Err(Refused::NoSuchOperation),
        ),
    ];
    for (phase, event, want) in table {
        assert_eq!(transition(phase, event), want, "{event:?} in {phase:?}");
    }
}

#[test]
fn the_actions_of_a_workspace_are_the_ones_its_phase_admits() {
    use Action::{
        AddRepository, Attach, Delete, GrowRootDisk, Reclaim, ResetRootDisk, Start, Stop,
    };
    let table: [(WorkspaceStatus, &[Action]); 8] = [
        (
            S::Created,
            &[Start, Delete, Reclaim, ResetRootDisk, GrowRootDisk],
        ),
        (S::Starting, &[]),
        (
            S::Running,
            &[Stop, Attach, Reclaim, GrowRootDisk, AddRepository],
        ),
        (S::Draining, &[]),
        (S::Paused, &[Reclaim, GrowRootDisk]),
        (
            S::Stopped,
            &[Start, Delete, Reclaim, ResetRootDisk, GrowRootDisk],
        ),
        (
            S::Crashed,
            &[Start, Delete, Reclaim, ResetRootDisk, GrowRootDisk],
        ),
        (
            S::VolumeMissing,
            &[Start, Delete, ResetRootDisk, GrowRootDisk],
        ),
    ];
    for (status, want) in table {
        assert_eq!(actions(&record(status, None)), want, "idle {status}");
        for op in OPERATIONS {
            assert_eq!(
                actions(&record(status, Some(op))),
                [],
                "{status} while {op}"
            );
        }
    }
}

#[test]
fn an_editor_or_terminal_for_a_busy_workspace_says_what_it_is_doing_not_that_it_is_stopped() {
    let busy = record(S::Running, Some(Operation::Cloning));
    says(&admit(&busy, Action::Attach), "web is busy (");
    let shown = format!("{:?}", admit(&busy, Action::Attach));
    assert!(!shown.contains("not running"), "{shown}");
}

#[test]
fn a_refusal_says_why_in_the_words_of_the_operation() {
    says(
        &admit(&record(S::Running, None), Action::Start),
        "web is already running",
    );
    says(
        &admit(&record(S::Stopped, None), Action::Stop),
        "web is not running (stopped)",
    );
    says(
        &admit(&record(S::Stopped, None), Action::Attach),
        "start it first",
    );
    says(
        &admit(&record(S::Starting, None), Action::Reclaim),
        "try again in a moment",
    );
    says(
        &admit(&record(S::VolumeMissing, None), Action::Reclaim),
        "volume is missing; restore it or delete the workspace",
    );
    says(
        &admit(&record(S::Running, None), Action::ResetRootDisk),
        "stop it before resetting its root disk",
    );
    says(
        &admit(&record(S::Draining, None), Action::Delete),
        "web is draining; stop it before deleting it",
    );
    says(
        &admit(&record(S::Stopped, None), Action::AddRepository),
        "web is stopped; start it to add a repository",
    );
    says(
        &admit(&record(S::Running, Some(Stopping)), Action::Reclaim),
        "web is busy (stopping); wait for it to finish",
    );
}

#[test]
fn begin_marks_the_record_and_a_refused_begin_leaves_it_alone() {
    let mut r = record(S::Stopped, None);
    begin(&mut r, Starting).unwrap();
    assert_eq!((r.status, r.busy), (S::Starting, Some(Starting)));
    let before = r.clone();
    says(&begin(&mut r, Stopping), "busy (starting)");
    assert_eq!(r, before);
    assert_eq!(r.apply(Move::End(Outcome::Done)), idle(S::Running));
    assert_eq!((r.status, r.busy), (S::Running, None));
}
