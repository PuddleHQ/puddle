// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a workspace's SSH endpoint serves clients with: the booted sandbox, plus a way to tell
//! the user when direct SSH stops working in a way the `ssh` client can't show.

use std::future::Future;
use std::sync::Arc;

use puddle_boot::{GatedError, GatedSandbox};
use puddle_compute::{Sandbox, SshStream};
use puddle_ssh::SshTarget;
use puddle_types::{Event, EventSink, WorkspaceId};

/// Tells the user, and the log, that `workspace`'s direct SSH failed.
pub(crate) fn report_failure(events: &dyn EventSink, workspace: &WorkspaceId, reason: String) {
    tracing::warn!(workspace = %workspace, %reason, "direct SSH failed");
    events.emit(Event::WorkspaceSshFailed {
        workspace: workspace.clone(),
        reason,
    });
}

/// Tells the user, and the log, that `workspace`'s direct SSH accepts clients again.
pub(crate) fn report_recovered(events: &dyn EventSink, workspace: &WorkspaceId) {
    tracing::info!(workspace = %workspace, "direct SSH accepts connections again");
    events.emit(Event::WorkspaceSshRecovered {
        workspace: workspace.clone(),
    });
}

/// Tells the user, and the log, that `workspace` got no SSH endpoint because of `error`.
pub(crate) fn report_no_endpoint(
    events: &dyn EventSink,
    workspace: &WorkspaceId,
    error: &dyn std::fmt::Display,
) {
    report_failure(
        events,
        workspace,
        format!("direct SSH has no endpoint ({error}); restart the workspace to try again"),
    );
}

/// A workspace's [`GatedSandbox`] as an SSH target whose failures become
/// [`Event::WorkspaceSshFailed`].
pub(crate) struct ReportingTarget<S: Sandbox> {
    gated: Arc<GatedSandbox<S>>,
    workspace: WorkspaceId,
    events: Arc<dyn EventSink>,
}

impl<S: Sandbox> ReportingTarget<S> {
    pub(crate) fn new(
        gated: Arc<GatedSandbox<S>>,
        workspace: WorkspaceId,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            gated,
            workspace,
            events,
        }
    }
}

impl<S: Sandbox> SshTarget for ReportingTarget<S> {
    type Error = GatedError;

    fn label(&self) -> String {
        self.gated.label()
    }

    fn admit(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.gated.admit()
    }

    fn failed(&self, reason: &str) {
        report_failure(self.events.as_ref(), &self.workspace, reason.to_owned());
    }

    fn recovered(&self) {
        report_recovered(self.events.as_ref(), &self.workspace);
    }

    fn serve<T: SshStream>(
        &self,
        stream: T,
    ) -> impl Future<Output = Result<(), GatedError>> + Send {
        self.gated.serve(stream)
    }
}

#[cfg(test)]
mod tests;
