// SPDX-License-Identifier: AGPL-3.0-or-later
use std::os::windows::io::AsHandle;
use std::process::Command;

use super::*;

#[test]
fn this_process_and_its_children_are_in_the_job() {
    contain_this_process().unwrap();
    contain_this_process().unwrap();
    let job = JOB.get().unwrap();
    assert!(job.contains_current_process().unwrap());
    // A child created without flags inherits the job.
    let mut child = Command::new("cmd")
        .args(["/c", "ping", "-n", "3", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let inside = job.contains(child.as_handle()).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(inside, "child process is not in the job");
    release_kill_on_close().unwrap();
}
