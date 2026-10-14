// SPDX-License-Identifier: GPL-3.0-or-later
//! What a workspace's SSH endpoint serves clients with: the booted sandbox, plus a way to tell
//! the user when direct SSH stops working in a way the `ssh` client can't show.

use std::future::Future;
use std::sync::Arc;

use puddle_boot::{GatedError, GatedSandbox};
use puddle_compute::{Sandbox, SshStream};
use puddle_ssh::SshTarget;
use puddle_types::{Event, EventSink, WorkspaceName};

/// Tells the user, and the log, that `workspace`'s direct SSH failed.
pub(crate) fn report_failure(events: &dyn EventSink, workspace: &WorkspaceName, reason: String) {
    tracing::warn!(workspace = %workspace, %reason, "direct SSH failed");
    events.emit(Event::WorkspaceSshFailed {
        workspace: workspace.clone(),
        reason,
    });
}

/// A workspace's [`GatedSandbox`] as an SSH target whose failures become
/// [`Event::WorkspaceSshFailed`].
pub(crate) struct ReportingTarget<S: Sandbox> {
    gated: Arc<GatedSandbox<S>>,
    workspace: WorkspaceName,
    events: Arc<dyn EventSink>,
}

impl<S: Sandbox> ReportingTarget<S> {
    pub(crate) fn new(
        gated: Arc<GatedSandbox<S>>,
        workspace: WorkspaceName,
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

    fn serve<T: SshStream>(
        &self,
        stream: T,
    ) -> impl Future<Output = Result<(), GatedError>> + Send {
        self.gated.serve(stream)
    }
}

#[cfg(test)]
mod tests {
    use puddle_types::CollectingSink;

    use super::*;

    #[test]
    fn a_failure_reaches_the_event_stream_with_its_workspace_and_reason() {
        let sink = CollectingSink::default();
        let workspace = WorkspaceName::new("box").unwrap();
        report_failure(&sink, &workspace, "the endpoint died".into());
        assert_eq!(
            sink.take(),
            [Event::WorkspaceSshFailed {
                workspace,
                reason: "the endpoint died".into(),
            }]
        );
    }
}
