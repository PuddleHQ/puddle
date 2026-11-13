// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn down_and_failed_refuse_at_once_ready_passes() {
    let gate = Gate::default();
    assert_eq!(gate.state(), GateState::Down);
    let long = Duration::from_secs(60);
    assert_eq!(gate.wait_ready(long).await, Err(NotReady::Down));
    let failure = BootFailure::Exited {
        code: 1,
        stderr: "puddle-boot: error: x".into(),
    };
    gate.set(GateState::Failed(failure.clone()));
    assert_eq!(
        gate.wait_ready(long).await,
        Err(NotReady::Failed(failure.clone()))
    );
    gate.set(GateState::Ready);
    assert_eq!(gate.wait_ready(long).await, Ok(()));
    gate.close();
    assert_eq!(gate.state(), GateState::Down);
}

#[tokio::test(start_paused = true)]
async fn booting_waits_until_settled_or_times_out() {
    let gate = Gate::new();
    gate.set(GateState::Booting);
    let limit = Duration::from_secs(5);
    assert_eq!(
        gate.wait_ready(limit).await,
        Err(NotReady::Timeout { waited: limit })
    );
    let waiter = {
        let gate = gate.clone();
        tokio::spawn(async move { gate.wait_ready(limit).await })
    };
    tokio::task::yield_now().await;
    gate.set(GateState::Ready);
    assert_eq!(waiter.await.unwrap(), Ok(()));
}

#[test]
fn refusals_read_well() {
    assert_eq!(NotReady::Down.to_string(), "workspace is not running");
    let e = GatedError::NotReady {
        sandbox: "a".into(),
        reason: NotReady::Timeout {
            waited: Duration::from_secs(2),
        },
    };
    assert_eq!(
        e.to_string(),
        r#"workspace "a": workspace is still booting after 2s"#
    );
}
