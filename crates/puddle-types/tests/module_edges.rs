// SPDX-License-Identifier: GPL-3.0-or-later
//! Edges of the smaller modules that the unit tests next to the code leave open: the exact length
//! limits of host names, the path of an absolute-form request target, the descriptions of the
//! local-address categories, the empty check of a guest environment and when a prefix clear is
//! announced. Each assertion is the result a caller sees at the public API.

use std::sync::Arc;

use puddle_types::{
    CollectingSink, Event, GuestEnv, Host, LocalCategory, Problem, Problems, request_path,
};

#[test]
fn an_absolute_form_target_keeps_its_path_whatever_the_authority_length() {
    assert_eq!(request_path("http://h/x"), "/x");
    assert_eq!(request_path("https://h/x/y"), "/x/y");
    assert_eq!(request_path("ftp://a"), "/");
    assert_eq!(request_path("https://user:pw@h.example/p?q#f"), "/p");
}

#[test]
fn a_host_name_may_have_exactly_253_characters_and_a_label_exactly_63() {
    let label = "a".repeat(63);
    let name_253 = format!("{label}.{label}.{label}.{}", "b".repeat(61));
    assert_eq!(name_253.len(), 253);
    assert!(Host::parse_normalised(&name_253).is_ok());
    let name_254 = format!("{label}.{label}.{label}.{}", "b".repeat(62));
    assert_eq!(name_254.len(), 254);
    assert!(Host::parse_normalised(&name_254).is_err());

    let host = Host::parse_normalised(&format!("{label}.example")).unwrap();
    assert_eq!(host.to_string(), format!("{label}.example"));
    assert!(Host::parse_normalised(&format!("{}.example", "a".repeat(64))).is_err());
}

#[test]
fn every_local_category_has_its_own_description() {
    let described: Vec<_> = LocalCategory::ALL.iter().map(|c| c.describe()).collect();
    assert_eq!(
        described,
        [
            "host loopback",
            "local/private network",
            "link-local",
            "cloud metadata",
            "special-use address"
        ]
    );
}

#[test]
fn a_guest_environment_is_empty_until_a_variable_is_set() {
    let mut env = GuestEnv::new();
    assert!(env.is_empty());
    assert_eq!(env.len(), 0);
    env.set("A", "1").unwrap();
    assert!(!env.is_empty());
    assert_eq!(env.len(), 1);
}

#[test]
fn clearing_a_prefix_is_announced_only_when_a_problem_went() {
    let sink = Arc::new(CollectingSink::default());
    let problems = Problems::new(sink.clone());
    problems.raise(Problem::new("git-locks:box", "a", "b"));
    assert_eq!(sink.take(), [Event::ProblemsChanged {}]);

    problems.clear_prefix("other:");
    assert_eq!(problems.list().len(), 1);
    assert_eq!(sink.take(), []);

    problems.clear_prefix("git-locks:");
    assert_eq!(problems.list().len(), 0);
    assert_eq!(sink.take(), [Event::ProblemsChanged {}]);

    problems.clear_prefix("git-locks:");
    assert_eq!(sink.take(), []);
}
