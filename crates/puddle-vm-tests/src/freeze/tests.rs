// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(target_os = "linux")]
use std::time::Duration;

use super::*;

#[cfg(target_os = "linux")]
const WAIT: Duration = Duration::from_secs(10);

#[test]
fn unix_commands_signal_the_pid() {
    assert_eq!(
        command_line(false, Action::Freeze, 42),
        ("kill", vec!["-STOP".to_owned(), "42".to_owned()])
    );
    assert_eq!(
        command_line(false, Action::Thaw, 42),
        ("kill", vec!["-CONT".to_owned(), "42".to_owned()])
    );
}

#[test]
fn windows_commands_call_the_matching_ntdll_function() {
    let (program, args) = command_line(true, Action::Freeze, 4242);
    assert_eq!(program, "powershell");
    assert_eq!(&args[..3], ["-NoProfile", "-NonInteractive", "-Command"]);
    assert!(args[3].contains("GetProcessById(4242)"), "{}", args[3]);
    assert!(args[3].contains("[Puddle.Nt]::NtSuspendProcess("));
    assert!(!args[3].contains("[Puddle.Nt]::NtResumeProcess("));
    let (_, thaw) = command_line(true, Action::Thaw, 7);
    assert!(thaw[3].contains("GetProcessById(7)"));
    assert!(thaw[3].contains("[Puddle.Nt]::NtResumeProcess("));
    assert!(!thaw[3].contains("[Puddle.Nt]::NtSuspendProcess("));
}

#[test]
fn a_heartbeat_with_one_long_gap_is_one_gap() {
    let ticks = [100, 101, 102, 103, 400, 401, 402];
    assert_eq!(
        Gaps::of(&ticks),
        Gaps {
            count: 1,
            longest: 297
        }
    );
}

#[test]
fn jitter_is_not_a_gap_and_the_edge_is_exact() {
    assert_eq!(
        Gaps::of(&[1, 2, 3]),
        Gaps {
            count: 0,
            longest: 1
        }
    );
    // A gap of exactly GAP_OVER is jitter; one more is a gap.
    assert_eq!(Gaps::of(&[10, 12, 14]).count, 0);
    assert_eq!(Gaps::of(&[10, 13]).count, 1);
    assert_eq!(
        Gaps::of(&[5]),
        Gaps {
            count: 0,
            longest: 0
        }
    );
    assert_eq!(
        Gaps::of(&[]),
        Gaps {
            count: 0,
            longest: 0
        }
    );
}

#[test]
fn the_pause_bar_is_one_gap_of_about_the_pause() {
    let one = |longest| Gaps { count: 1, longest };
    assert!(one(297).match_pause(300));
    assert!(one(270).match_pause(300), "30 s short is the edge");
    assert!(!one(269).match_pause(300));
    assert!(one(420).match_pause(300), "120 s long is the edge");
    assert!(!one(421).match_pause(300));
    assert!(
        !Gaps {
            count: 2,
            longest: 300
        }
        .match_pause(300)
    );
    assert!(
        !Gaps {
            count: 0,
            longest: 300
        }
        .match_pause(300)
    );
}

#[test]
fn ticks_skip_lines_that_are_not_numbers() {
    assert_eq!(parse_ticks("1\n 2 \nx\n\n3\n17"), vec![1, 2, 3, 17]);
}

/// Whether the kernel lists `pid` as stopped (state `T`).
#[cfg(target_os = "linux")]
fn stopped(pid: u32) -> bool {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    // The state letter follows the `)` that ends the command name.
    stat.rsplit_once(')')
        .unwrap()
        .1
        .trim_start()
        .starts_with('T')
}

#[cfg(target_os = "linux")]
fn sleeper() -> std::process::Child {
    Command::new("sleep").arg("60").spawn().unwrap()
}

/// A real process, frozen and thawed: the kernel says stopped, then not.
#[cfg(target_os = "linux")]
#[test]
fn a_frozen_process_stops_and_a_thawed_one_runs() {
    let mut child = sleeper();
    let pid = child.id();
    let mut frozen = FrozenProcess::freeze(pid).unwrap();
    puddle_test_kit::eventually_blocking("the process is stopped", WAIT, || stopped(pid));
    frozen.thaw().unwrap();
    puddle_test_kit::eventually_blocking("the process runs again", WAIT, || !stopped(pid));
    child.kill().unwrap();
    child.wait().unwrap();
}

/// The drop thaws what a panicking test left frozen.
#[cfg(target_os = "linux")]
#[test]
fn dropping_a_frozen_process_thaws_it() {
    let mut child = sleeper();
    let pid = child.id();
    let frozen = FrozenProcess::freeze(pid).unwrap();
    puddle_test_kit::eventually_blocking("the process is stopped", WAIT, || stopped(pid));
    drop(frozen);
    puddle_test_kit::eventually_blocking("the drop thawed it", WAIT, || !stopped(pid));
    child.kill().unwrap();
    child.wait().unwrap();
}

/// A thawed process is not thawed again by the drop (it would signal a reused pid).
#[cfg(target_os = "linux")]
#[test]
fn a_thawed_process_is_left_alone_by_the_drop() {
    let mut child = sleeper();
    let pid = child.id();
    let mut frozen = FrozenProcess::freeze(pid).unwrap();
    frozen.thaw().unwrap();
    // Stopped by someone else after the thaw: the drop must not undo it.
    let again = Command::new("kill")
        .args(["-STOP", &pid.to_string()])
        .status()
        .unwrap();
    assert!(again.success());
    puddle_test_kit::eventually_blocking("stopped again", WAIT, || stopped(pid));
    drop(frozen);
    assert!(stopped(pid), "the drop of a thawed process sent a signal");
    Command::new("kill")
        .args(["-CONT", &pid.to_string()])
        .status()
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn freezing_a_process_that_does_not_exist_fails_with_the_reason() {
    // u32::MAX - 1 is no process id on any OS here.
    let err = FrozenProcess::freeze(u32::MAX - 1).unwrap_err();
    assert!(matches!(err, HarnessError::Freeze { pid, .. } if pid == u32::MAX - 1));
}
