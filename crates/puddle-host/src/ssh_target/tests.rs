// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::CollectingSink;

use super::*;

#[test]
fn a_failure_reaches_the_event_stream_with_its_workspace_and_reason() {
    let sink = CollectingSink::default();
    let workspace = WorkspaceId::new("box").unwrap();
    report_failure(&sink, &workspace, "the endpoint died".into());
    report_no_endpoint(&sink, &workspace, &"address in use");
    report_recovered(&sink, &workspace);
    assert_eq!(
            sink.take(),
            [
                Event::WorkspaceSshFailed {
                    workspace: workspace.clone(),
                    reason: "the endpoint died".into(),
                },
                Event::WorkspaceSshFailed {
                    workspace: workspace.clone(),
                    reason: "direct SSH has no endpoint (address in use); restart the workspace to try again"
                        .into(),
                },
                Event::WorkspaceSshRecovered { workspace },
            ]
        );
}
