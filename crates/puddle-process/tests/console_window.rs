// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the user sees when the desktop app starts a console program: a process without a console
//! (as the app, a Windows GUI program, is) starts this test binary as a probe, and the probe
//! reports whether its console has a window on the desktop.
#![cfg(windows)]
#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "the probe asks Win32 whether its console has a window"
    )
)]
#![cfg_attr(
    windows,
    expect(clippy::unwrap_used, reason = "a failed step fails the test")
)]

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows_sys::Win32::System::Console::GetConsoleWindow;
use windows_sys::Win32::System::Threading::DETACHED_PROCESS;
use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

const ROLE: &str = "PUDDLE_PROCESS_TEST_ROLE";
const OUT: &str = "PUDDLE_PROCESS_TEST_OUT";
const HELPER: &str = "helper_role";

fn start(cmd: &mut Command, role: &str, out: &Path) -> std::process::ExitStatus {
    cmd.args([HELPER, "--exact", "--test-threads", "1"])
        .env(ROLE, role)
        .env(OUT, out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
}

/// Not a test of its own: the role a copy of this binary plays for the tests below.
#[test]
fn helper_role() {
    // skip-ok: a plain test run only lists this entry; the tests below start it with a role
    let Some(role) = std::env::var(ROLE).ok() else {
        return;
    };
    let out = PathBuf::from(std::env::var_os(OUT).unwrap());
    let exe = std::env::current_exe().unwrap();
    let status = match role.as_str() {
        "parent-through-helper" => start(&mut puddle_process::command(&exe), "probe", &out),
        "parent-plain" => start(&mut Command::new(&exe), "probe", &out),
        "probe" => {
            // SAFETY: no arguments; returns this process's console window handle, or null.
            let window = unsafe { GetConsoleWindow() };
            // SAFETY: a window handle the system just returned; a stale one makes it return 0.
            let visible = !window.is_null() && unsafe { IsWindowVisible(window) } != 0;
            let seen = match (window.is_null(), visible) {
                (true, _) => "no window",
                (false, false) => "hidden window",
                (false, true) => "visible window",
            };
            std::fs::write(&out, seen).unwrap();
            return;
        }
        other => panic!("unknown role {other}"),
    };
    assert!(status.success(), "the probe failed: {status}");
}

/// Starts this binary as `role` without a console (`DETACHED_PROCESS`), as the desktop app runs,
/// and returns what its probe child saw.
fn seen_from_a_process_without_a_console(role: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("seen");
    let exe = std::env::current_exe().unwrap();
    let status = start(
        Command::new(exe).creation_flags(DETACHED_PROCESS),
        role,
        &out,
    );
    assert!(status.success(), "the parent failed: {status}");
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn a_console_program_started_through_the_helper_gets_no_window() {
    assert_eq!(
        seen_from_a_process_without_a_console("parent-through-helper"),
        "no window"
    );
}

/// The control: started plainly, the same child gets a console window, so the probe can see one
/// and the test above cannot pass because it is blind.
#[test]
fn a_console_program_started_plainly_gets_a_window() {
    assert_ne!(
        seen_from_a_process_without_a_console("parent-plain"),
        "no window"
    );
}
