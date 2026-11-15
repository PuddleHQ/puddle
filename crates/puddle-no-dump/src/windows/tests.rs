// SPDX-License-Identifier: AGPL-3.0-or-later
use std::process::Command;

use super::{exclude, set_excluded};
use crate::Outcome;

const LIST: &str = r"HKCU\Software\Microsoft\Windows\Windows Error Reporting\ExcludedApplications";

/// Whether the exclusion list of the current user names `name`, read with `reg query`.
fn listed(name: &str) -> bool {
    Command::new("reg")
        .args(["query", LIST, "/v", name])
        .output()
        .expect("run reg.exe")
        .status
        .success()
}

#[test]
fn a_name_is_listed_by_hardening_and_gone_again_with_the_opt_out() {
    let name = format!("puddle-no-dump-test-{}.exe", std::process::id());
    assert!(!listed(&name), "the name starts unlisted");
    assert_eq!(
        exclude(std::slice::from_ref(&name), false),
        Outcome::Protected
    );
    let after_hardening = listed(&name);
    assert_eq!(exclude(std::slice::from_ref(&name), true), Outcome::Allowed);
    assert!(after_hardening, "WER lists the executable after hardening");
    assert!(!listed(&name), "the opt-out removes it");
}

#[test]
fn hardening_twice_is_the_same_as_once() {
    let name = format!("puddle-no-dump-twice-{}.exe", std::process::id());
    let names = [name.clone()];
    assert_eq!(exclude(&names, false), Outcome::Protected);
    assert_eq!(exclude(&names, false), Outcome::Protected);
    let listed_once = listed(&name);
    set_excluded(&name, false).expect("remove the test entry");
    assert!(listed_once);
}
