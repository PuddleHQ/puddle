// SPDX-License-Identifier: AGPL-3.0-or-later
use std::future::Future;

use puddle_compute::{ExecOutput, ExecRequest, SshStream};

use super::*;

/// A sandbox whose exec and stop never answer (a hung runtime).
struct Hung(SandboxName);

impl Sandbox for Hung {
    fn name(&self) -> &SandboxName {
        &self.0
    }

    fn owns_lifecycle(&self) -> bool {
        true
    }

    fn status(&self) -> impl Future<Output = Result<WorkspaceStatus, ComputeError>> + Send {
        std::future::ready(Ok(WorkspaceStatus::Running))
    }

    fn stop(&self) -> impl Future<Output = Result<(), ComputeError>> + Send {
        std::future::pending()
    }

    fn exec(
        &self,
        _request: ExecRequest,
    ) -> impl Future<Output = Result<ExecOutput, ComputeError>> + Send {
        std::future::pending()
    }

    fn serve_ssh<S: SshStream>(
        &self,
        _stream: S,
    ) -> impl Future<Output = Result<(), ComputeError>> + Send {
        std::future::pending()
    }
}

#[test]
fn every_way_a_stop_goes_wrong_has_a_reason_and_a_clean_stop_has_none() {
    let failed = ComputeError::Runtime {
        op: "stop",
        message: "stuck".to_owned(),
    };
    assert_eq!(StopOutcome::Stopped.problem(), None);
    assert!(
        StopOutcome::Failed(failed)
            .problem()
            .unwrap()
            .contains("stuck")
    );
    assert!(
        StopOutcome::TimedOut
            .problem()
            .unwrap()
            .contains("no answer")
    );
    assert!(StopOutcome::Forced.problem().unwrap().contains("by force"));
    assert!(
        StopOutcome::Panicked
            .problem()
            .unwrap()
            .contains("panicked")
    );
}

#[tokio::test(start_paused = true)]
async fn a_hung_trim_and_stop_time_out_in_order() {
    let config = ShutdownConfig {
        trim_timeout: Duration::from_secs(3),
        stop_timeout: Duration::from_secs(5),
    };
    let hung = Hung(SandboxName::new("hung").unwrap());
    let start = tokio::time::Instant::now();
    let (trim, stop) = trim_and_stop(&hung, &[], &config).await;
    assert!(matches!(trim, TrimOutcome::Error(ref e) if e.contains("no answer")));
    assert_eq!(stop, StopOutcome::TimedOut);
    assert_eq!(start.elapsed(), Duration::from_secs(8));
    assert!(hung.owns_lifecycle());
    // A runtime that does not say counts as not forced.
    assert!(!hung.stopped_by_force());
}
