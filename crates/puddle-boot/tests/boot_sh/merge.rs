// SPDX-License-Identifier: AGPL-3.0-or-later
//! Files merged into a guest file instead of replaced, and provider steps.

use super::*;

const DOCKER: &str = "/root/.docker/config.json";

fn merged(path: &str) -> GuestFile {
    let spec = MergeSpec::new(
        MergeFormat::Json,
        vec![MergeEntry::json(
            &["proxies", "default"],
            &serde_json::json!({"httpProxy": "http://172.17.0.1:3128"}),
        )],
    )
    .unwrap();
    GuestFile::merged(GuestPath::new(path).unwrap(), spec).with_mode(FileMode::new(0o600))
}

fn plan_with(files: Vec<GuestFile>) -> BootPlan {
    BootPlan::builder(&ImageConfig::default())
        .files(files)
        .build()
        .unwrap()
}

/// The merge tool's calls for the files a test lists (not VS Code's Machine settings, which
/// every plan merges).
fn merge_calls(fr: &FakeRoot) -> Vec<String> {
    fr.calls()
        .into_iter()
        .filter(|c| c.starts_with("merge-file") && !c.contains(MACHINE_SETTINGS_GUEST))
        .collect()
}

#[test]
fn merged_files_go_through_the_merge_tool_and_lose_only_puddles_keys_when_dropped() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let file = merged(DOCKER);
        let out = fr.run(&plan_with(vec![file.clone()]));
        assert!(out.status.success(), "{}", show(&out));
        // The log says which file got Puddle's lines.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains(&format!(
                "{DOCKER}: Puddle's own lines were changed (everything else in the file is as it was)"
            )),
            "{}",
            show(&out)
        );
        assert_eq!(
            merge_calls(&fr),
            [format!("merge-file apply {DOCKER} 0600")]
        );
        let puddle_types::ApplyKind::Merge(spec) = file.apply() else {
            panic!("not merged")
        };
        let sent: MergeSpec = serde_json::from_str(&fr.read("/merge-spec")).unwrap();
        assert_eq!(&sent, spec);
        assert!(fr.path("/root/.docker").is_dir());
        assert_eq!(
            fr.read("/var/lib/puddle/merge-files"),
            format!("{MACHINE_SETTINGS_GUEST}\n{DOCKER}\n")
        );
        assert!(!fr.read("/var/lib/puddle/boot-files").contains(DOCKER));

        // The provider stops listing it: the tool removes Puddle's keys; the file isn't deleted
        // by the hook.
        let out = fr.run(&plan_with(vec![]));
        assert!(out.status.success(), "{}", show(&out));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(&format!(
                "Puddle's keys removed from {DOCKER} (no longer in the plan): changed"
            )),
            "{stdout}"
        );
        assert_eq!(
            merge_calls(&fr).last().unwrap(),
            &format!("merge-file remove {DOCKER}")
        );
        assert_eq!(
            fr.read("/var/lib/puddle/merge-files"),
            format!("{MACHINE_SETTINGS_GUEST}\n")
        );
        let out = fr.run(&plan_with(vec![]));
        assert!(out.status.success());
        assert_eq!(merge_calls(&fr).len(), 2, "nothing left to remove");
    }
}

#[test]
fn a_kept_file_stays_as_an_earlier_plan_merged_it_and_is_still_puddles_to_remove() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let kept = || GuestFile::kept(GuestPath::new(DOCKER).unwrap());
        // Never merged: keeping it does nothing and records nothing.
        let out = fr.run(&plan_with(vec![kept()]));
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(merge_calls(&fr), Vec::<String>::new());
        assert!(!fr.read("/var/lib/puddle/merge-files").contains(DOCKER));

        let out = fr.run(&plan_with(vec![merged(DOCKER)]));
        assert!(out.status.success(), "{}", show(&out));
        // Kept: the merge tool is not asked to set or remove anything, and the record stays.
        let out = fr.run(&plan_with(vec![kept()]));
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(
            merge_calls(&fr),
            [format!("merge-file apply {DOCKER} 0600")]
        );
        assert_eq!(
            fr.read("/var/lib/puddle/merge-files"),
            format!("{MACHINE_SETTINGS_GUEST}\n{DOCKER}\n")
        );
        // A later plan without it removes Puddle's keys as for any merged file.
        let out = fr.run(&plan_with(vec![]));
        assert!(out.status.success(), "{}", show(&out));
        assert_eq!(
            merge_calls(&fr).last().unwrap(),
            &format!("merge-file remove {DOCKER}")
        );
    }
}

#[test]
fn a_file_the_tool_cannot_parse_is_left_and_the_boot_goes_on() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(
        fr.path("/merge-answer"),
        "left: the file is not valid JSON: EOF while parsing at line 1 column 1\n",
    )
    .unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "{DOCKER} left as it is: the file is not valid JSON: EOF while parsing"
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{MACHINE_SETTINGS_GUEST} left as it is")),
        "{stdout}"
    );
    assert!(stdout.contains("files applied (2 changed)"), "{stdout}");
}

#[test]
fn a_failing_or_odd_merge_tool_fails_the_boot_with_its_message() {
    let fr = FakeRoot::new(&shells()[0]);
    std::fs::write(fr.path("/merge-answer"), "fail").unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("cannot merge {MACHINE_SETTINGS_GUEST}: disk full")),
        "{stderr}"
    );
    std::fs::write(fr.path("/merge-answer"), "maybe").unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unexpected answer from the merge tool"),
        "{stderr}"
    );
    std::fs::remove_file(fr.path("/puddle/puddle-agent")).unwrap();
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("the merge tool /puddle/puddle-agent is missing"),
        "{stderr}"
    );
}

#[test]
fn switching_a_file_between_replace_and_merge_keeps_it() {
    let fr = FakeRoot::new(&shells()[0]);
    // The earlier whole-file Docker config, then the merged one (an upgrade): the file stays.
    assert!(
        fr.run(&plan_with(vec![file(DOCKER, b"{}")]))
            .status
            .success()
    );
    let out = fr.run(&plan_with(vec![merged(DOCKER)]));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(fr.read(DOCKER), "{}");
    assert_eq!(
        merge_calls(&fr),
        [format!("merge-file apply {DOCKER} 0600")]
    );
    // Back to whole-file: the tool only forgets its record.
    let out = fr.run(&plan_with(vec![file(DOCKER, b"{\"x\":1}")]));
    assert!(out.status.success(), "{}", show(&out));
    assert_eq!(
        merge_calls(&fr).last().unwrap(),
        &format!("merge-file forget {DOCKER}")
    );
    assert_eq!(fr.read(DOCKER), "{\"x\":1}");
}

#[test]
fn without_the_merge_tool_the_removal_waits_for_a_later_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&plan_with(vec![merged(DOCKER)])).status.success());
    std::fs::rename(fr.path("/puddle/puddle-agent"), fr.path("/agent.saved")).unwrap();
    let bare = BootPlan::builder(&ImageConfig::default())
        .no_agent()
        .build()
        .unwrap();
    // Every plan merges VS Code's Machine settings, so a missing tool now fails the boot
    // before anything is removed: the record of Docker's keys stays for a later boot.
    let out = fr.run(&bare);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("the merge tool /puddle/puddle-agent is missing")
    );
    assert_eq!(
        fr.read("/var/lib/puddle/merge-files"),
        format!("{MACHINE_SETTINGS_GUEST}\n{DOCKER}\n")
    );
    std::fs::rename(fr.path("/agent.saved"), fr.path("/puddle/puddle-agent")).unwrap();
    assert!(fr.run(&bare).status.success());
    assert_eq!(
        merge_calls(&fr).last().unwrap(),
        &format!("merge-file remove {DOCKER}")
    );
}

#[test]
fn provider_steps_run_every_boot_after_the_ca_trigger_and_fail_the_boot_when_they_fail() {
    let step = "/usr/local/lib/puddle/step.sh";
    let plan = |script: &str| {
        BootPlan::builder(&image(&[], &["bash"]))
            .no_agent()
            .file(file(CA, b"CA\n"))
            .file(file(step, script.as_bytes()))
            .step(GuestPath::new(step).unwrap())
            .build()
            .unwrap()
    };
    let ok = "echo step >>\"$PUDDLE_ROOT/calls\"; echo merged\n";
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        for _ in 0..2 {
            let out = fr.run(&plan(ok));
            assert!(out.status.success(), "{}", show(&out));
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(stdout.contains(&format!("step {step}: merged")), "{stdout}");
        }
        // Every boot, after update-ca-certificates (which ran on the first boot only).
        assert_eq!(fr.calls().iter().filter(|c| *c == "step").count(), 2);
        let calls = fr.calls();
        let first_step = calls.iter().position(|c| c == "step").unwrap();
        let update = calls
            .iter()
            .position(|c| c == "update-ca-certificates")
            .unwrap();
        assert!(update < first_step, "{calls:?}");

        let out = fr.run(&plan("echo 'no bundle here' >&2; exit 4\n"));
        assert_eq!(out.status.code(), Some(1), "{}", show(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(&format!(
                "puddle-boot: error: step {step} failed: no bundle here"
            )),
            "{stderr}"
        );
        assert!(!fr.path("/run/puddle/boot.done").exists());
    }
}
