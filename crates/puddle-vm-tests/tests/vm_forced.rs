// SPDX-License-Identifier: GPL-3.0-or-later
//! Temporary: forces a failure and a stuck test on the repro branch to see them named.
#[test]
fn vm_forced_failure() {
    panic!("forced failure");
}
#[test]
fn vm_forced_stuck() {
    std::thread::sleep(std::time::Duration::from_secs(600));
}
