// SPDX-License-Identifier: AGPL-3.0-or-later
use super::command;

#[test]
fn the_command_runs_the_named_program_with_its_arguments() {
    let exe = std::env::current_exe().unwrap();
    let mut cmd = command(&exe);
    cmd.args(["--list", "--format", "terse"]);
    assert_eq!(cmd.get_program(), exe.as_os_str());
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let listed = String::from_utf8(out.stdout).unwrap();
    assert!(
        listed.contains("the_command_runs_the_named_program_with_its_arguments: test"),
        "the child is this test binary and lists its tests: {listed}"
    );
}
