// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_types::{CollectingSink, Event};

use super::*;

fn name(s: &str) -> WorkspaceName {
    WorkspaceName::new(s).unwrap()
}

fn desk() -> (Arc<CollectingSink>, Arc<Problems>, ReportDesk) {
    let sink = Arc::new(CollectingSink::default());
    let problems = Arc::new(Problems::new(sink.clone()));
    (sink, problems.clone(), ReportDesk::new(problems))
}

fn exited(status: u8, tail: &str) -> AgentReport {
    AgentReport::EntrypointExited {
        status,
        tail: tail.to_owned(),
    }
}

fn dns(kind: StubDnsTrouble) -> AgentReport {
    AgentReport::StubDns { kind, count: 1 }
}

fn changes(sink: &CollectingSink) -> usize {
    sink.take()
        .into_iter()
        .filter(|e| *e == Event::ProblemsChanged {})
        .count()
}

#[test]
fn a_failed_entrypoint_is_a_problem_on_its_workspace_in_the_boot_hooks_words() {
    let (_, problems, desk) = desk();
    let box_ = name("box");
    desk.begin(&box_);
    desk.report(&box_, exited(0, "done"));
    assert_eq!(problems.list(), Vec::<Problem>::new(), "exit 0 did its job");
    desk.report(&box_, exited(137, "Killed"));
    let raised = problems.list();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0], entrypoint_problem(&box_, "137: Killed"));
    assert_eq!(raised[0].key, "entrypoint:box");
    assert_eq!(raised[0].workspace, Some(box_.clone()));
    assert!(
        raised[0]
            .detail
            .contains("It exited with status 137: Killed.")
    );
    assert!(raised[0].detail.contains("/run/puddle/entrypoint.log"));
}

#[test]
fn an_entrypoint_with_no_output_says_so_like_the_boot_hook() {
    let (_, problems, desk) = desk();
    let box_ = name("box");
    desk.begin(&box_);
    desk.report(&box_, exited(1, ""));
    assert_eq!(problems.list(), [entrypoint_problem(&box_, "1: no output")]);
}

#[test]
fn guest_text_in_the_problem_is_cut_and_has_no_control_characters() {
    let problem = entrypoint_problem(&name("box"), &format!("1: {}", "\u{1b}[2J".repeat(500)));
    assert!(!problem.detail.chars().any(char::is_control));
    assert!(problem.detail.chars().count() < MAX_FAILURE_CHARS + 300);
}

#[test]
fn stub_dns_trouble_that_fails_a_tool_is_a_problem_naming_each_kind_once() {
    let (sink, problems, desk) = desk();
    let box_ = name("box");
    desk.begin(&box_);
    let _ = sink.take();
    desk.report(&box_, dns(StubDnsTrouble::UdpDropped));
    desk.report(&box_, dns(StubDnsTrouble::NoLookupSlot));
    desk.report(&box_, dns(StubDnsTrouble::TcpRefused));
    assert_eq!(
        problems.list(),
        Vec::<Problem>::new(),
        "the client asks again for these"
    );
    desk.report(&box_, dns(StubDnsTrouble::LookupTimedOut));
    desk.report(&box_, dns(StubDnsTrouble::LookupTimedOut));
    desk.report(&box_, dns(StubDnsTrouble::NoStandInAddress));
    let raised = problems.list();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].key, "stub-dns:box");
    assert_eq!(raised[0].title, "Name lookups in box are failing");
    assert!(raised[0].detail.contains("did not answer them in time"));
    assert!(raised[0].detail.contains("ran out of addresses"));
    assert!(!raised[0].detail.contains("left out"));
    assert_eq!(
        changes(&sink),
        2,
        "one change per new kind, none per repeat"
    );
}

#[test]
fn reports_count_only_between_a_start_and_a_stop() {
    let (_, problems, desk) = desk();
    let box_ = name("box");
    desk.report(&box_, exited(1, "before any start"));
    assert_eq!(problems.list(), Vec::<Problem>::new());
    desk.begin(&box_);
    desk.pause(&box_);
    desk.report(&box_, exited(143, "Terminated"));
    desk.report(&box_, dns(StubDnsTrouble::LookupFailed));
    assert_eq!(
        problems.list(),
        Vec::<Problem>::new(),
        "a stop ends the entrypoint"
    );
    desk.resume(&box_);
    desk.report(&box_, dns(StubDnsTrouble::LookupFailed));
    assert_eq!(problems.list().len(), 1, "a failed stop runs on");
    desk.end(&box_);
    assert_eq!(
        problems.list(),
        Vec::<Problem>::new(),
        "the DNS problem ends with the machine"
    );
    desk.report(&box_, exited(1, "after the stop"));
    assert_eq!(problems.list(), Vec::<Problem>::new());
}

#[test]
fn the_entrypoint_problem_outlives_a_stop_and_ends_at_the_next_start_or_a_delete() {
    let (_, problems, desk) = desk();
    let box_ = name("box");
    desk.begin(&box_);
    desk.report(&box_, exited(2, "bad"));
    desk.report(&box_, dns(StubDnsTrouble::RecordsLeftOut));
    desk.pause(&box_);
    desk.end(&box_);
    let keys: Vec<_> = problems.list().into_iter().map(|p| p.key).collect();
    assert_eq!(keys, ["entrypoint:box"]);
    desk.begin(&box_);
    assert_eq!(problems.list(), Vec::<Problem>::new());
    desk.report(&box_, exited(2, "bad"));
    desk.forget(&box_);
    assert_eq!(problems.list(), Vec::<Problem>::new());
}

#[test]
fn reports_from_one_workspace_never_touch_the_problems_of_another() {
    let (_, problems, desk) = desk();
    let (a, b) = (name("a"), name("b"));
    desk.begin(&a);
    desk.begin(&b);
    problems.raise(entrypoint_problem(&b, "1: b's own"));
    desk.report(&a, exited(9, "b"));
    desk.end(&a);
    desk.forget(&a);
    assert_eq!(problems.list(), [entrypoint_problem(&b, "1: b's own")]);
}

/// HG-37 at the host's end: whatever a hostile guest sends during one run, each problem changes
/// within its bound, so the notices and `problems_changed` events stay bounded too. The control
/// stream's own limits (line length, rate, one stream per session) come on top and are tested in
/// `puddle-agent-proto`.
#[test]
fn hostile_hg37_a_flood_of_reports_changes_each_problem_a_bounded_number_of_times() {
    let (sink, problems, desk) = desk();
    let (box_, other) = (name("box"), name("other"));
    desk.begin(&box_);
    desk.begin(&other);
    let _ = sink.take();
    for i in 0..100_000_u32 {
        let status = u8::try_from(i % 255).unwrap() + 1;
        desk.report(&box_, exited(status, &format!("cause {i}")));
        let kind = StubDnsTrouble::ALL[(i as usize) % StubDnsTrouble::ALL.len()];
        desk.report(&box_, AgentReport::StubDns { kind, count: i });
        desk.report(&box_, dns(StubDnsTrouble::Unknown));
    }
    // One change for the entrypoint (the first failure), one per kind that fails a tool (four).
    assert_eq!(changes(&sink), 5);
    let raised = problems.list();
    assert_eq!(raised.len(), 2);
    assert_eq!(raised[0], entrypoint_problem(&box_, "1: cause 0"));
    assert!(raised.iter().all(|p| p.workspace == Some(box_.clone())));
    assert_eq!(desk.runs().get(&box_).unwrap().dns.len(), 4);
    assert!(desk.runs().get(&other).unwrap().dns.is_empty());
}
