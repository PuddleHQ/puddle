// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent the hook starts: restarts, settings and the capture switch.

use super::*;

#[test]
fn agent_is_back_within_a_second_after_kill_9() {
    let fr = FakeRoot::new(&shells()[0]);
    assert!(fr.run(&full_plan("CA\n")).status.success());
    let first = fr.agent_pids()[0];
    kill9(first);
    eventually_blocking(
        "the supervisor restarts the agent",
        Duration::from_secs(1),
        || {
            let pids = fr.agent_pids();
            if pids.len() == 2 && alive(pids[1]) {
                Ok(())
            } else {
                Err(format!("pids {pids:?}"))
            }
        },
    );
}

#[test]
fn the_agent_starts_with_the_plans_settings_and_a_rerun_with_new_ones_restarts_it() {
    for sh in shells() {
        let fr = FakeRoot::new(&sh);
        let plan = |value: &str| {
            BootPlan::builder(&ImageConfig::default())
                .agent_env("PUDDLE_AGENT_TEST", value)
                .build()
                .unwrap()
        };
        let starts = |n: usize, what: &str| {
            eventually_blocking(what, Duration::from_secs(5), || {
                let saw = fr.agent_saw();
                if saw.len() == n {
                    Ok(())
                } else {
                    Err(format!("starts {saw:?}"))
                }
            });
        };
        // Spaces, quotes, `$` and a backslash reach the agent as they are.
        let odd = "a b 'q' \"d\" $HOME \\ end";
        assert!(fr.run(&plan(odd)).status.success());
        assert_eq!(fr.agent_saw(), [odd]);
        let supervisor = fr.recorded_pid("supervisor.pid").unwrap();
        // The same settings again (a re-adopted boot): the running agent stays.
        assert!(fr.run(&plan(odd)).status.success());
        assert_eq!(fr.agent_pids().len(), 1, "an unchanged agent was restarted");
        // New settings while the supervisor runs (a re-adopted boot after the user changed
        // them): the agent restarts with them in this boot, under the same supervisor.
        let out = fr.run(&plan("second"));
        assert!(out.status.success(), "{}", show(&out));
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("agent settings changed"),
            "{}",
            show(&out)
        );
        starts(2, "the agent restarts with the new settings");
        assert_eq!(fr.agent_saw(), [odd, "second"]);
        assert!(!alive(fr.agent_pids()[0]), "the old agent still runs");
        assert_eq!(fr.recorded_pid("supervisor.pid"), Some(supervisor));
        assert!(!fr.path("/run/puddle/agent.env.new").exists());
        // A plan without settings clears them.
        assert!(
            fr.run(&BootPlan::builder(&ImageConfig::default()).build().unwrap())
                .status
                .success()
        );
        starts(3, "the agent restarts a third time");
        assert_eq!(fr.agent_saw().last().map(String::as_str), Some("unset"));
    }
}

#[test]
fn a_capture_switch_on_a_rerun_takes_effect_in_that_boot() {
    let fr = FakeRoot::new(&shells()[0]);
    let off = BootPlan::builder(&ImageConfig::default())
        .no_capture()
        .build()
        .unwrap();
    assert!(fr.run(&off).status.success());
    let on = BootPlan::builder(&ImageConfig::default()).build().unwrap();
    let out = fr.run(&on);
    assert!(out.status.success(), "{}", show(&out));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(fr.agent_capture(), ["off", "on"], "{stdout}");
    assert!(stdout.contains("stub DNS answering"), "{stdout}");
}
