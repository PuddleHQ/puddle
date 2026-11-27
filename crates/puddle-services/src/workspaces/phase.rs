// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspace lifecycle as one state machine: the [`Phase`] a workspace is in, what each
//! [`Move`] makes of it ([`transition`]), and which user [`Action`]s a phase admits
//! ([`actions`]).
//!
//! The real service and the in-memory one change a workspace's status and busy mark only through
//! [`transition`] (by way of [`begin`] and [`WorkspaceRecord::apply`]), and the API publishes
//! [`actions`] on every workspace, so the host, the fake and the UI read one table.

use puddle_types::WorkspaceStatus;

use super::{Operation, WorkspaceError, WorkspaceRecord};

/// Where a workspace is in its lifecycle: [`WorkspaceRecord::status`] and
/// [`WorkspaceRecord::busy`] read as one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No operation runs; the status is where the last one, or the runtime, left it.
    Idle(WorkspaceStatus),
    /// The operation runs; the status is what the workspace shows meanwhile.
    Busy(Operation, WorkspaceStatus),
}

/// Something that moves a workspace from one phase to the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    /// The operation was asked for. A create begins on its fresh record, which is
    /// `Idle(Created)`.
    Begin(Operation),
    /// The operation's request went away before its work started.
    Withdraw,
    /// A root disk grow that has to restart the workspace begins stopping it.
    RestartStops,
    /// That restart's stop is done and the start begins.
    RestartStarts,
    /// The operation's work ended.
    End(Outcome),
    /// The runtime reports the workspace's VM down.
    VmDied,
}

/// How an operation's work ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It did what it was asked.
    Done,
    /// It failed.
    Failed,
    /// It failed because the workspace's volume is gone.
    VolumeGone,
}

/// What a transition leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// The workspace is in this phase now.
    Phase(Phase),
    /// The workspace leaves the list: its create failed or was dropped, or its delete is done.
    Removed,
}

/// Why a phase does not take an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Another operation runs.
    Busy(Operation),
    /// The operation needs another status.
    Status(WorkspaceStatus),
    /// The event belongs to an operation, and none runs (or a different one does).
    NoSuchOperation,
}

/// What the user can ask of a workspace. [`actions`] lists the ones its phase admits now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Start it.
    Start,
    /// Stop it.
    Stop,
    /// Open an editor in it.
    Attach,
    /// Delete it.
    Delete,
    /// Give freed disk space back to the host.
    Reclaim,
    /// Remove its root disk so the next start makes a fresh one.
    ResetRootDisk,
    /// Make its root disk bigger.
    GrowRootDisk,
    /// Add a repository to it, or clone one of its repositories again.
    AddRepository,
}

impl Action {
    /// Every action, in the order [`actions`] lists them.
    pub const ALL: [Self; 8] = [
        Self::Start,
        Self::Stop,
        Self::Attach,
        Self::Delete,
        Self::Reclaim,
        Self::ResetRootDisk,
        Self::GrowRootDisk,
        Self::AddRepository,
    ];

    /// The operation the action begins; `None` for one that changes no state (attach).
    #[must_use]
    pub fn operation(self) -> Option<Operation> {
        match self {
            Self::Start => Some(Operation::Starting),
            Self::Stop => Some(Operation::Stopping),
            Self::Attach => None,
            Self::Delete => Some(Operation::Deleting),
            Self::Reclaim => Some(Operation::Reclaiming),
            Self::ResetRootDisk => Some(Operation::ResettingRootDisk),
            Self::GrowRootDisk => Some(Operation::GrowingRootDisk),
            Self::AddRepository => Some(Operation::Cloning),
        }
    }
}

/// Whether an idle workspace with `status` can begin `op`.
fn admits(op: Operation, status: WorkspaceStatus) -> bool {
    use WorkspaceStatus::{Created, Draining, Running, Starting, VolumeMissing};
    match op {
        Operation::Creating => status == Created,
        Operation::Starting | Operation::ResettingRootDisk | Operation::Deleting => {
            status.is_down()
        }
        Operation::Stopping | Operation::Cloning => status == Running,
        // Both wait out a start or a stop in progress; a reclaim trims the volume, so it needs
        // one. A grow works on the root disk alone, which goes with the sandbox.
        Operation::Reclaiming => !matches!(status, Starting | Draining | VolumeMissing),
        Operation::GrowingRootDisk => !matches!(status, Starting | Draining),
    }
}

/// The status a workspace shows while `op` runs, from the one it had when `op` began.
fn entered(op: Operation, status: WorkspaceStatus) -> WorkspaceStatus {
    match op {
        Operation::Starting => WorkspaceStatus::Starting,
        Operation::Stopping => WorkspaceStatus::Draining,
        _ => status,
    }
}

/// Where `op`'s end leaves a workspace that showed `status` when the work ended. A start in
/// progress (a start, a create, a grow's restart) that worked is running and one that failed is
/// crashed, or volume-missing when the volume is gone; a stop in progress that failed leaves the
/// VM up.
fn ended(op: Operation, status: WorkspaceStatus, outcome: Outcome) -> Next {
    use WorkspaceStatus::{Crashed, Draining, Running, Starting, Stopped, VolumeMissing};
    let idle = |s| Next::Phase(Phase::Idle(s));
    match (op, outcome) {
        (Operation::Creating, Outcome::Done) => idle(Running),
        (Operation::Creating, _) | (Operation::Deleting, Outcome::Done) => Next::Removed,
        (Operation::Stopping, Outcome::Done) => idle(Stopped),
        (_, Outcome::Done) if status == Starting => idle(Running),
        (_, Outcome::Failed) if status == Starting => idle(Crashed),
        (_, Outcome::VolumeGone) if status == Starting => idle(VolumeMissing),
        (_, Outcome::Failed | Outcome::VolumeGone) if status == Draining => idle(Running),
        // Everything else ends where the work left it.
        (_, Outcome::Done | Outcome::Failed | Outcome::VolumeGone) => idle(status),
    }
}

/// The lifecycle's one transition function: what `event` makes of a workspace in `phase`.
///
/// # Errors
///
/// [`Refused`] when the phase does not take the event: an operation asked for while another runs
/// or in a status it cannot begin in, or an operation's event while it does not run.
pub fn transition(phase: Phase, event: Move) -> Result<Next, Refused> {
    use WorkspaceStatus::{Draining, Running, Starting};
    let to = |p| Ok(Next::Phase(p));
    match (phase, event) {
        (Phase::Busy(running, _), Move::Begin(_)) => Err(Refused::Busy(running)),
        (Phase::Idle(status), Move::Begin(op)) if admits(op, status) => {
            to(Phase::Busy(op, entered(op, status)))
        }
        (Phase::Idle(status), Move::Begin(_)) => Err(Refused::Status(status)),
        (Phase::Busy(Operation::Creating, _), Move::Withdraw) => Ok(Next::Removed),
        // Whatever set the status since (a crash the monitor saw) still holds.
        (Phase::Busy(_, status), Move::Withdraw) => to(Phase::Idle(status)),
        (Phase::Busy(Operation::GrowingRootDisk, Running), Move::RestartStops) => {
            to(Phase::Busy(Operation::GrowingRootDisk, Draining))
        }
        (Phase::Busy(Operation::GrowingRootDisk, Draining), Move::RestartStarts) => {
            to(Phase::Busy(Operation::GrowingRootDisk, Starting))
        }
        (Phase::Busy(op, status), Move::End(outcome)) => Ok(ended(op, status, outcome)),
        (Phase::Idle(Running), Move::VmDied) => to(Phase::Idle(WorkspaceStatus::Crashed)),
        // Down already, or an operation runs: its end decides.
        (phase, Move::VmDied) => to(phase),
        (
            Phase::Idle(_),
            Move::Withdraw | Move::RestartStops | Move::RestartStarts | Move::End(_),
        )
        | (Phase::Busy(..), Move::RestartStops | Move::RestartStarts) => {
            Err(Refused::NoSuchOperation)
        }
    }
}

impl WorkspaceRecord {
    /// The workspace's phase: its status and busy mark as one value.
    #[must_use]
    pub fn phase(&self) -> Phase {
        match self.busy {
            Some(op) => Phase::Busy(op, self.status),
            None => Phase::Idle(self.status),
        }
    }

    /// Applies `event` to the record and says what came of it. A [`Next::Removed`] leaves the
    /// record as it was: the caller takes it off the list.
    ///
    /// # Errors
    ///
    /// [`Refused`] as [`transition`]; the record is unchanged.
    pub fn apply(&mut self, event: Move) -> Result<Next, Refused> {
        let next = transition(self.phase(), event)?;
        if let Next::Phase(phase) = next {
            (self.busy, self.status) = match phase {
                Phase::Idle(status) => (None, status),
                Phase::Busy(op, status) => (Some(op), status),
            };
        }
        Ok(next)
    }
}

/// Marks `record` busy with `op` when its phase admits it, with the status `op` shows while it
/// runs.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] that says why it cannot begin; the record is unchanged.
pub fn begin(record: &mut WorkspaceRecord, op: Operation) -> Result<(), WorkspaceError> {
    match record.apply(Move::Begin(op)) {
        Ok(_) => Ok(()),
        Err(why) => Err(refusal(record, op, why)),
    }
}

/// Whether `record` can take `action` now.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] that says why not.
pub fn admit(record: &WorkspaceRecord, action: Action) -> Result<(), WorkspaceError> {
    match action.operation() {
        Some(op) => match transition(record.phase(), Move::Begin(op)) {
            Ok(_) => Ok(()),
            Err(why) => Err(refusal(record, op, why)),
        },
        None => match record.phase() {
            Phase::Idle(WorkspaceStatus::Running) => Ok(()),
            Phase::Busy(running, _) => Err(WorkspaceError::Conflict(format!(
                "{} is busy ({running}); wait for it to finish",
                record.name
            ))),
            Phase::Idle(_) => Err(WorkspaceError::Conflict(format!(
                "{} is not running; start it first",
                record.name
            ))),
        },
    }
}

/// The actions `record`'s phase admits now, in [`Action::ALL`]'s order.
#[must_use]
pub fn actions(record: &WorkspaceRecord) -> Vec<Action> {
    Action::ALL
        .into_iter()
        .filter(|&action| admit(record, action).is_ok())
        .collect()
}

/// The words for an operation that `record` refused.
fn refusal(record: &WorkspaceRecord, op: Operation, why: Refused) -> WorkspaceError {
    let name = &record.name;
    WorkspaceError::Conflict(match why {
        Refused::Busy(running) => format!("{name} is busy ({running}); wait for it to finish"),
        Refused::NoSuchOperation => format!("{name} is not {op}"),
        Refused::Status(status) => match op {
            Operation::Starting => format!("{name} is already {status}"),
            Operation::Stopping => format!("{name} is not running ({status})"),
            Operation::Reclaiming if status == WorkspaceStatus::VolumeMissing => {
                format!("{name}'s volume is missing; restore it or delete the workspace")
            }
            Operation::Reclaiming | Operation::GrowingRootDisk => {
                format!("{name} is {status}; try again in a moment")
            }
            Operation::ResettingRootDisk => {
                format!("{name} is {status}; stop it before resetting its root disk")
            }
            Operation::Deleting => format!("{name} is {status}; stop it before deleting it"),
            Operation::Cloning => format!("{name} is {status}; start it to add a repository"),
            Operation::Creating => format!("{name} is {status}; only a new workspace is created"),
        },
    })
}

#[cfg(test)]
mod tests;
