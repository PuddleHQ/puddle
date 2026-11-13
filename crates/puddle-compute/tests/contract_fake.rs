// SPDX-License-Identifier: GPL-3.0-or-later
//! The contract suite against `FakeRuntime`: the fake must pass every case.
#![expect(
    clippy::unwrap_used,
    reason = "the env helper runs outside #[test] but only in tests"
)]

use puddle_compute::contract::{self, ContractEnv};
use puddle_compute::fake::FakeRuntime;
use puddle_types::ImageRef;

async fn env() -> ContractEnv<FakeRuntime> {
    ContractEnv::new(
        FakeRuntime::new(),
        ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
    )
}

puddle_compute::contract_tests!(env);

#[test]
fn macro_runs_every_case() {
    assert_eq!(CONTRACT_CASES, contract::CASES);
}

#[tokio::test]
async fn run_all_passes_on_a_shared_fake() {
    let env = env().await;
    let report = contract::run_all(&env).await;
    assert!(report.all_passed(), "{:#?}", report.failed);
    assert_eq!(report.passed.len(), contract::CASES.len());
}

#[tokio::test]
async fn cases_leave_nothing_behind() {
    use puddle_compute::Runtime;
    let env = env().await;
    assert!(contract::run_all(&env).await.all_passed());
    assert_eq!(env.runtime.list().await.unwrap(), []);
    assert_eq!(env.runtime.list_volumes().await.unwrap(), []);
    assert_eq!(
        env.runtime.stale_dirs().await.unwrap(),
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn an_unknown_case_fails_cleanly() {
    let err = contract::run_case("no_such_case", &env().await)
        .await
        .unwrap_err();
    assert_eq!(err.case, "no_such_case");
}

#[tokio::test]
#[should_panic(expected = "contract case")]
async fn assert_case_panics_on_failure() {
    contract::assert_case("no_such_case", env().await).await;
}

/// The suite must not be vacuous: a runtime that deviates fails the matching cases.
async fn failing_cases(rt: FakeRuntime) -> Vec<String> {
    let env = ContractEnv::new(rt, ImageRef::new(FakeRuntime::DEBIAN).unwrap());
    let report = contract::run_all(&env).await;
    report.failed.into_iter().map(|f| f.case).collect()
}

#[tokio::test]
async fn the_suite_catches_lost_exit_codes() {
    use puddle_compute::ExecOutput;
    let rt = FakeRuntime::new();
    rt.on_exec(
        |_: &mut puddle_compute::fake::ExecContext<'_>, r: &puddle_compute::ExecRequest| {
            (r.program == "sh").then(|| ExecOutput::new(0, "", ""))
        },
    );
    let failed = failing_cases(rt).await;
    assert!(
        failed.contains(&"exec_exit_codes_are_exact".to_owned()),
        "{failed:?}"
    );
    assert!(
        failed.contains(&"signal_killed_exec_is_a_failure".to_owned()),
        "{failed:?}"
    );
}

#[tokio::test]
async fn the_suite_catches_a_writable_file_or_directory_mount() {
    use puddle_compute::ExecOutput;
    let rt = FakeRuntime::new();
    rt.on_exec(
        |_: &mut puddle_compute::fake::ExecContext<'_>, r: &puddle_compute::ExecRequest| {
            (r.program == "tee" && r.args.iter().any(|a| a.starts_with("/puddle-ct/")))
                .then(|| ExecOutput::new(0, "", ""))
        },
    );
    let failed = failing_cases(rt).await;
    assert_eq!(
        failed,
        ["file_mount_is_read_only", "dir_mount_is_read_only"]
    );
}

#[tokio::test]
async fn the_suite_reports_what_a_refused_mount_write_said_when_it_is_not_a_read_only_error() {
    use puddle_compute::ExecOutput;
    let rt = FakeRuntime::new();
    rt.on_exec(
        |_: &mut puddle_compute::fake::ExecContext<'_>, r: &puddle_compute::ExecRequest| {
            (r.program == "tee" && r.args.iter().any(|a| a.starts_with("/puddle-ct/")))
                .then(|| ExecOutput::new(1, "", "Permission denied"))
        },
    );
    let failed = failing_cases(rt).await;
    assert_eq!(
        failed,
        ["file_mount_is_read_only", "dir_mount_is_read_only"]
    );
}

#[tokio::test]
async fn the_suite_catches_a_runtime_that_always_fails() {
    use puddle_compute::ComputeError;
    use puddle_compute::fake::{Fault, Op};
    let rt = FakeRuntime::new();
    rt.inject(
        Op::Create,
        Fault::always(ComputeError::Runtime {
            op: "create",
            message: "down".into(),
        }),
    );
    let failed = failing_cases(rt).await;
    assert!(failed.len() >= 20, "{failed:?}");
}

/// A runtime whose guest answers `probe` with `output` instead of what the fake guest would.
fn guest_answers(
    probe: puddle_compute::probes::Probe,
    output: puddle_compute::ExecOutput,
    only_arg: Option<&'static str>,
) -> FakeRuntime {
    let rt = FakeRuntime::new();
    rt.on_exec(
        move |_: &mut puddle_compute::fake::ExecContext<'_>, r: &puddle_compute::ExecRequest| {
            let (found, args) = puddle_compute::probes::Probe::of(r)?;
            (found == probe && only_arg.is_none_or(|a| args.get(1) == Some(&a)))
                .then(|| output.clone())
        },
    );
    rt
}

#[tokio::test]
async fn the_suite_catches_a_guest_that_reaches_out() {
    use puddle_compute::ExecOutput;
    use puddle_compute::probes::Probe;
    let leaks = [
        (
            Probe::NoDirectNetwork,
            ExecOutput::new(1, "DNS resolved\n", ""),
            "hg01_guest_has_no_direct_network",
        ),
        (
            Probe::Icmp,
            ExecOutput::new(
                0,
                "127.0.0.1 reply\n1.1.1.1 reply\n8.8.8.8 no-reply\n9.9.9.9 no-reply\n",
                "",
            ),
            "hg01_guest_has_no_direct_network",
        ),
        (
            Probe::Icmp,
            ExecOutput::new(
                0,
                "1.1.1.1 no-reply\n8.8.8.8 no-reply\n9.9.9.9 no-reply\n",
                "",
            ),
            "hg01_guest_has_no_direct_network",
        ),
        (
            Probe::Packet,
            ExecOutput::new(
                0,
                "control-packet captured\nraw-control sent\nraw6 1.1.1.1 sent\n",
                "",
            ),
            "hg01_raw_and_packet_sockets_reach_no_network",
        ),
        (
            Probe::Packet,
            ExecOutput::new(0, "raw-control sent\n", ""),
            "hg01_raw_and_packet_sockets_reach_no_network",
        ),
    ];
    for (probe, output, case) in leaks {
        let failed = failing_cases(guest_answers(probe, output.clone(), None)).await;
        assert_eq!(failed, [case], "{probe:?} answering {output:?}");
    }
}

#[tokio::test]
async fn the_suite_catches_a_guest_that_reaches_a_vsock_address_it_should_not() {
    use puddle_compute::ExecOutput;
    use puddle_compute::probes::Probe;
    let accepted = |port: u32| ExecOutput::new(0, format!("accepted {port}\n"), "cid: x\n");
    // The first case sweeps the host and CIDs 0, 1 and 3; the second sweeps the other CIDs and the
    // guest's own, which is 3.
    for (cid, cases) in [
        ("0", &["hg02_only_the_routed_vsock_port_accepts"][..]),
        ("1", &["hg02_only_the_routed_vsock_port_accepts"][..]),
        (
            "3",
            &[
                "hg02_only_the_routed_vsock_port_accepts",
                "hg02_no_other_vsock_address_accepts",
            ][..],
        ),
        ("4", &["hg02_no_other_vsock_address_accepts"][..]),
    ] {
        let failed =
            failing_cases(guest_answers(Probe::VsockSweep, accepted(5000), Some(cid))).await;
        assert_eq!(failed, cases, "CID {cid} accepting");
    }
    // An unrouted host port accepting, and a routed port whose bytes never arrive.
    let failed = failing_cases(guest_answers(Probe::VsockSweep, accepted(4000), Some("2"))).await;
    assert_eq!(
        failed,
        [
            "hg02_only_the_routed_vsock_port_accepts",
            "hg02_no_other_vsock_address_accepts"
        ]
    );
    let failed = failing_cases(guest_answers(Probe::VsockSweep, accepted(5000), Some("2"))).await;
    assert_eq!(
        failed,
        [
            "hg02_only_the_routed_vsock_port_accepts",
            "hg02_no_other_vsock_address_accepts"
        ]
    );
    let garbled = ExecOutput::new(0, "it works\n", "");
    let failed = failing_cases(guest_answers(Probe::VsockSweep, garbled, None)).await;
    assert_eq!(
        failed,
        [
            "hg02_only_the_routed_vsock_port_accepts",
            "hg02_no_other_vsock_address_accepts"
        ]
    );
}

#[tokio::test]
async fn a_probe_that_exits_non_zero_fails_its_case_with_what_it_said() {
    use puddle_compute::ExecOutput;
    use puddle_compute::probes::Probe;
    for (probe, case) in [
        (Probe::Icmp, "hg01_guest_has_no_direct_network"),
        (
            Probe::Packet,
            "hg01_raw_and_packet_sockets_reach_no_network",
        ),
        (Probe::VsockSweep, "hg02_only_the_routed_vsock_port_accepts"),
        (Probe::VsockSweep, "hg02_no_other_vsock_address_accepts"),
        (Probe::NoDirectNetwork, "hg01_guest_has_no_direct_network"),
    ] {
        let rt = guest_answers(probe, ExecOutput::new(1, "", "probe broke"), None);
        let env = ContractEnv::new(rt, ImageRef::new(FakeRuntime::DEBIAN).unwrap());
        let err = contract::run_case(case, &env).await.unwrap_err();
        assert!(err.message.contains("probe broke"), "{case}: {err}");
    }
}

#[tokio::test]
async fn a_runtime_that_cannot_run_a_probe_fails_its_case() {
    use puddle_compute::ComputeError;
    use puddle_compute::fake::{Fault, Op};
    let rt = FakeRuntime::new();
    // The first exec of the case is its shell script; the second is the probe.
    rt.inject(
        Op::Exec,
        Fault::once(ComputeError::Runtime {
            op: "exec",
            message: "agent gone".into(),
        })
        .after(1),
    );
    let env = ContractEnv::new(rt, ImageRef::new(FakeRuntime::DEBIAN).unwrap());
    let err = contract::run_case("hg01_guest_has_no_direct_network", &env)
        .await
        .unwrap_err();
    assert!(err.message.contains("agent gone"), "{err}");
}
