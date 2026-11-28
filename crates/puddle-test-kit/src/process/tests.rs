// SPDX-License-Identifier: AGPL-3.0-or-later
use std::process::Command;
use std::time::Duration;

use super::*;

fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[test]
fn a_child_that_ignores_the_stop_is_killed_after_the_bound() {
    let child = Command::new("sleep").arg("600").spawn().unwrap();
    let pid = child.id();
    let guard = ProcessGuard::new(child, None).with_stop_wait(Duration::from_millis(100));
    drop(guard);
    assert!(!alive(pid), "the child outlived its guard");
}

#[test]
fn a_child_that_exited_is_reaped_without_waiting() {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    drop(ProcessGuard::new(child, None));
    assert!(!alive(pid));
}

#[test]
fn a_panicking_test_still_ends_its_child() {
    let child = Command::new("sleep").arg("600").spawn().unwrap();
    let pid = child.id();
    let result = std::panic::catch_unwind(move || {
        let _guard = ProcessGuard::new(child, None).with_stop_wait(Duration::from_millis(100));
        panic!("a failed assert");
    });
    assert!(result.is_err());
    assert!(!alive(pid));
}
