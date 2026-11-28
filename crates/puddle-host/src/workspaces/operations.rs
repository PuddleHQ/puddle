// SPDX-License-Identifier: AGPL-3.0-or-later
//! The operation protocol: ticket, run, conclude and the audit of each.

use std::future::Future;
use std::sync::{MutexGuard, PoisonError};

use puddle_compute::Runtime;
use puddle_services::workspaces::phase::{Move, Next, Outcome, begin};
use puddle_services::{
    workspaces::Operation, workspaces::WorkspaceError, workspaces::WorkspaceRecord,
};
use puddle_store::{Actor, NewWorkspaceOp, WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{WorkspaceId, WorkspaceStatus, WorkspaceStep};

use tokio::task::JoinSet;

use super::{HostWorkspaces, Leftovers, Running, Ticket, not_found};

use crate::op_error::OpError;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// Begins `op` on `id` when its phase admits it (see [`puddle_services::workspaces::phase`]): the busy mark
    /// goes on and the status becomes the one `op` shows while it runs.
    pub(super) fn accept(
        &self,
        id: &WorkspaceId,
        op: Operation,
    ) -> Result<Ticket<R>, WorkspaceError> {
        let mut state = self.state();
        if state.closed {
            return Err(WorkspaceError::Unavailable(
                "Puddle is shutting down".to_owned(),
            ));
        }
        let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
        begin(&mut slot.record, op)?;
        Ok(self.ticket(slot.record.clone(), op))
    }

    /// The ticket of an operation that has just marked `record` busy.
    pub(super) fn ticket(&self, record: WorkspaceRecord, op: Operation) -> Ticket<R> {
        Ticket {
            service: self.clone(),
            record,
            op,
            armed: true,
        }
    }

    /// Takes off the busy mark of an operation that never started its work. A create that never
    /// started leaves nothing to keep: its entry goes and its id is retired, like a failed
    /// create's. The status stays: whatever set it since (a crash the monitor saw) still holds.
    pub(super) fn withdraw(&self, id: &WorkspaceId, op: Operation) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        if slot.record.apply(Move::Withdraw) == Ok(Next::Removed) {
            state.slots.remove(id);
            state.retired.insert(id.clone());
            if let Err(e) = self.persist(&state) {
                // Harmless: the saved entry says "being created", so the next start drops it.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a create was dropped");
            }
            return;
        }
        slot.deleting = false;
        slot.delete_note = None;
        let record = slot.record.clone();
        drop(state);
        tracing::info!(workspace = %id, %op, "a request was dropped before its work began");
        // The mark is off: say so, as the end of an operation does.
        self.status_event(&record);
    }

    pub(super) fn tasks(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Spawns `work` on a task the service owns: it runs to its end whatever becomes of the
    /// request that started it, unless shutdown ends it.
    pub(super) fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        // Shutdown has taken the tasks to end them: work started after it would never be.
        if self.state().closed {
            return;
        }
        let mut tasks = self.tasks();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(work);
    }

    /// Runs `job` on a task the service owns and returns what it returns. A caller dropped while
    /// it waits leaves the job to finish: a maintenance sandbox the job booted is stopped and
    /// removed, and its hold on the volume given back, only once the job has done so. Shutdown
    /// waits for the job like for an operation.
    pub(super) async fn on_own_task<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let running = Running::new(&self.inner.running);
        self.spawn(async move {
            let _running = running;
            // Nobody waits any more when the request was dropped: the outcome goes with it.
            let _ = tx.send(job.await);
        });
        rx.await
            .map_err(|_| WorkspaceError::Unavailable("Puddle is shutting down".to_owned()))
    }

    /// Runs `work` on a task the service owns and settles the workspace when it ends. The task
    /// takes over the busy mark of `ticket`; nothing awaits between the two.
    pub(super) fn run<F>(&self, mut ticket: Ticket<R>, work: F)
    where
        F: Future<Output = Result<(), OpError>> + Send + 'static,
    {
        let this = self.clone();
        let (id, op) = (ticket.record.id.clone(), ticket.op);
        let running = Running::new(&self.inner.running);
        self.spawn(async move {
            let _running = running;
            let result = work.await;
            this.refresh_disk_use().await;
            this.conclude(&id, op, result);
        });
        ticket.armed = false;
    }

    /// Ends an operation: the busy mark goes, the state follows the outcome, and the last event
    /// of the operation says how it went.
    pub(super) fn conclude(&self, id: &WorkspaceId, op: Operation, result: Result<(), OpError>) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        slot.deleting = false;
        let delete_note = slot.delete_note.take();
        let leftovers = std::mem::replace(&mut slot.leftovers, Leftovers::Unknown);
        let volume_gone = std::mem::take(&mut slot.volume_missing);
        let name = slot.record.id.clone();
        let before = slot.record.status;
        let outcome = match &result {
            Ok(()) => Outcome::Done,
            Err(_) if volume_gone => Outcome::VolumeGone,
            Err(_) => Outcome::Failed,
        };
        let next = slot.record.apply(Move::End(outcome));
        // A failed create, and a delete either way, leave the list different from what is saved:
        // the failed create's entry goes, the failed delete's entry comes back unmarked, and the
        // finished delete's entry goes with its id retired.
        let save = matches!(
            (&result, op),
            (Err(_), Operation::Creating | Operation::Deleting) | (Ok(()), Operation::Deleting)
        );
        let mut result = result;
        let old_entry = slot.creating;
        let status_changed = slot.record.status != before;
        if op == Operation::Creating && outcome == Outcome::Done {
            slot.creating = false;
        }
        // A stop in progress that failed leaves the VM up: its reports go on.
        if before == WorkspaceStatus::Draining && slot.record.status == WorkspaceStatus::Running {
            self.inner.reports.resume(&name);
        }
        if next == Ok(Next::Removed) {
            // A failed create may leave rows and files keyed by its id: retired like a deleted
            // workspace's, so no later one meets them.
            state.slots.remove(id);
            state.retired.insert(id.clone());
            if op == Operation::Deleting && leftovers == Leftovers::Gone {
                state.cleaned.insert(id.clone());
            }
        }
        let record = state.slots.get(id).map(|s| s.record.clone());
        if save && let Err(e) = self.persist(&state) {
            if old_entry {
                // The entry still says "being created", so the next start drops it and removes
                // what is left of the volume: nothing the user worked on.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a failed create");
            } else {
                // The workspace is now missing from the saved list: the next start keeps its
                // volume and reports it, but doesn't list the workspace.
                result = result.map_err(|cause| OpError::NotSaved {
                    cause: Box::new(cause),
                    source: e,
                });
            }
        }
        drop(state);
        if let (true, Some(record)) = (status_changed, &record) {
            self.status_event(record);
        }
        self.audit_operation(&name, op, &result, delete_note);
        self.last_step(&name, op, result);
    }

    /// The event that ends an operation, `done` or `failed`. A create or a start that worked then
    /// sets up the managed tools in the background: the start is done whatever mise does, and a
    /// create's clones are on the volume by now.
    fn last_step(&self, name: &WorkspaceId, op: Operation, result: Result<(), OpError>) {
        match result {
            Ok(()) => {
                self.progress(name, WorkspaceStep::Done, None);
                if matches!(op, Operation::Creating | Operation::Starting) {
                    self.set_up_tools_later(name);
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %op, %reason, "workspace operation failed");
                self.progress(name, WorkspaceStep::Failed, Some(reason.to_string()));
            }
        }
    }

    /// Writes what an operation came to into the audit log: a create or a delete either way, a
    /// start or a stop only when it failed (a start that worked shows in the workspace's state).
    pub(super) fn audit_operation(
        &self,
        name: &WorkspaceId,
        op: Operation,
        result: &Result<(), OpError>,
        delete_note: Option<&'static str>,
    ) {
        let kind = match op {
            Operation::Creating => WorkspaceOpKind::Create,
            Operation::Deleting => WorkspaceOpKind::Delete,
            Operation::Starting if result.is_err() => WorkspaceOpKind::Start,
            Operation::Stopping if result.is_err() => WorkspaceOpKind::Stop,
            _ => return,
        };
        let (outcome, detail) = match result {
            Ok(()) => (
                WorkspaceOpOutcome::Succeeded,
                delete_note.map(str::to_owned),
            ),
            Err(reason) => (WorkspaceOpOutcome::Failed, Some(reason.to_string())),
        };
        self.record_op(kind, name, outcome, detail);
    }

    pub(super) fn record_op(
        &self,
        op: WorkspaceOpKind,
        name: &WorkspaceId,
        outcome: WorkspaceOpOutcome,
        detail: Option<String>,
    ) {
        let new = NewWorkspaceOp {
            op,
            workspace: Some(name.clone()),
            user: None,
            outcome,
            actor: Actor::Api,
            detail,
        };
        self.inner.store.note_workspace_op(&new);
    }
}
