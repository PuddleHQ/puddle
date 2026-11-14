// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn proc(pid: u32, parent: u32, created_s: u64) -> Proc {
    Proc {
        pid,
        parent,
        name: format!("p{pid}"),
        created: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(created_s)),
        cpu: Some(Duration::from_millis(u64::from(pid))),
    }
}

#[test]
fn the_tree_holds_the_root_its_children_and_theirs_and_no_one_else() {
    let all = [
        proc(1, 0, 0),
        proc(10, 1, 100),
        proc(11, 10, 101),
        proc(12, 11, 102),
        proc(20, 1, 100),
        proc(13, 10, 103),
    ];
    let pids: Vec<u32> = descendants(&all, 10).iter().map(|p| p.pid).collect();
    assert_eq!(pids, [10, 11, 13, 12]);
    assert_eq!(descendants(&all, 99).len(), 0);
}

#[test]
fn a_process_older_than_its_parent_id_is_not_its_child() {
    // Process 30 was started by an earlier process 10 that has ended; the id was reused.
    let all = [proc(10, 1, 100), proc(30, 10, 50), proc(31, 10, 150)];
    let pids: Vec<u32> = descendants(&all, 10).iter().map(|p| p.pid).collect();
    assert_eq!(pids, [10, 31]);
}

#[test]
fn a_process_without_a_creation_time_counts_as_a_child_and_its_start_is_unknown() {
    let mut child = proc(11, 10, 0);
    child.created = None;
    let all = [proc(10, 1, 100), child.clone(), proc(12, 10, 90)];
    assert_eq!(descendants(&all, 10).len(), 2);
    let mut early = proc(13, 10, 50);
    early.cpu = None;
    assert_eq!(
        describe(&[proc(10, 1, 100), child, early]),
        "  p10 pid 10 started +0.00s CPU 0.01s\n  p11 pid 11 started unknown CPU 0.01s\n  p13 pid 13 started before the first CPU unknown"
    );
}

#[test]
fn memory_is_given_in_megabytes_or_as_unknown() {
    assert_eq!(
        describe_memory(Some((3 << 20, 16 << 30))),
        "free memory: 3 of 16384 MB"
    );
    assert_eq!(describe_memory(None), "free memory: unknown on this OS");
}

#[test]
fn a_process_that_does_not_exist_has_no_tree_and_no_cpu_time() {
    assert_eq!(tree_cpu(u32::MAX), None);
}

#[test]
fn the_description_gives_each_start_relative_to_the_first_and_its_cpu() {
    let mut late = proc(11, 10, 160);
    late.cpu = None;
    let text = describe(&[proc(10, 1, 100), late]);
    assert_eq!(
        text,
        "  p10 pid 10 started +0.00s CPU 0.01s\n  p11 pid 11 started +60.00s CPU unknown"
    );
    assert!(describe(&[]).contains("not readable"));
}

#[test]
fn cpu_time_is_user_plus_system_ticks_counted_after_the_name() {
    // Field 2 holds a space and a parenthesis; utime and stime are fields 14 and 15.
    let line = "42 (mitm (dump) x) S 1 42 42 0 -1 4194560 100 0 0 0 55 17 0 0 20 0 1 0 100 0 0";
    assert_eq!(parse_cpu_time(line), Some(Duration::from_millis(720)));
    assert_eq!(parse_cpu_time("no name here"), None);
    assert_eq!(parse_cpu_time("1 (x) S 1"), None);
}

#[test]
#[cfg(target_os = "linux")]
fn a_proc_stat_line_gives_pid_parent_name_start_and_cpu() {
    let line = "42 (mitm (dump) x) S 7 42 42 0 -1 4194560 100 0 0 0 55 17 0 0 20 0 1 0 250 0 0";
    let boot = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
    let p = os::parse(line, Some(boot)).unwrap();
    assert_eq!(
        p,
        Proc {
            pid: 42,
            parent: 7,
            name: "mitm (dump) x".to_owned(),
            created: Some(boot + Duration::from_millis(2500)),
            cpu: Some(Duration::from_millis(720)),
        }
    );
}

/// A shell that starts a long child and waits for it: a tree of two.
fn shell_with_a_child() -> Child {
    let (shell, args, _) = programs::SHELL_WITH_A_CHILD;
    Command::new(shell)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The grandchild outlives a killed shell; it must hold none of the test's pipes.
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
}

#[test]
#[cfg(any(target_os = "linux", windows))]
fn a_running_tree_is_read_with_its_child_and_reported() {
    let mut shell = shell_with_a_child();
    let pid = shell.id();
    let found = puddle_test_kit::eventually_blocking(
        "the shell's child process",
        Duration::from_secs(30),
        || {
            let tree = tree(pid);
            (tree.len() >= 2).then_some(tree)
        },
    );
    let evidence = stall_evidence(&shell);
    let cpu = tree_cpu(pid);
    let summary = tree_summary(&shell);
    drop(shell.kill());
    drop(shell.wait());
    assert_eq!(found.first().map(|p| p.pid), Some(pid));
    assert_eq!(found.len(), 2, "{found:?}");
    let child = found.last().unwrap();
    let (_, _, expected) = programs::SHELL_WITH_A_CHILD;
    assert!(child.name.eq_ignore_ascii_case(expected), "{found:?}");
    assert!(child.created.is_some() && child.cpu.is_some(), "{found:?}");
    assert!(cpu.is_some());
    assert!(summary.contains(&format!("/{pid} ")), "{summary}");
    assert!(
        evidence.contains(&format!("pid {pid} started +0.00s")),
        "{evidence}"
    );
    assert!(evidence.contains("a trivial program"), "{evidence}");
    assert!(evidence.contains("to start and end"), "{evidence}");
    let (free, total) = os::memory().unwrap();
    assert!(0 < free && free <= total, "{free} of {total}");
    assert!(evidence.contains("free memory: "), "{evidence}");
}

#[test]
fn a_program_that_overruns_its_bound_is_reported() {
    let (program, args) = programs::IDLE;
    let text = time_to_end(program, args, Duration::from_millis(200));
    assert!(text.contains("had not ended after 0.2s"), "{text}");
    assert!(trivial_start().contains("to start and end"));
    assert!(
        time_to_end("no-such-program-puddle", &[], TRIVIAL_START_BOUND).contains("did not start")
    );
}

#[test]
#[cfg(not(any(target_os = "linux", windows)))]
fn without_a_process_table_there_is_no_tree() {
    assert!(tree(std::process::id()).is_empty());
    assert_eq!(tree_cpu(std::process::id()), None);
}
