// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn values_share_one_random_suffix_and_differ_by_label() {
    let (a, b) = (password("a"), password("b"));
    assert_ne!(a, b);
    assert!(a.ends_with(suffix()) && b.ends_with(suffix()));
    assert_eq!(suffix().len(), 16);
    assert!(suffix().bytes().all(|c| c.is_ascii_hexdigit()));
    assert!(canary("x").starts_with("CANARY-x-"));
}

#[test]
fn unique_names_differ_on_every_call_and_carry_the_run_suffix() {
    let (a, b) = (unique("pac"), unique("pac"));
    assert_ne!(a, b);
    for name in [&a, &b] {
        assert!(name.starts_with(&format!("pac-{}-", suffix())), "{name}");
    }
}

#[test]
fn a_clean_text_passes() {
    assert_absent("a reply", "nothing to see", &canary("clean"));
}

// The negative control: a canary that does appear fails the assertion, and the message
// names the leak without printing the value.
#[test]
#[should_panic(expected = "the reply leaked a fixture secret")]
fn a_planted_canary_fails_the_assertion() {
    let planted = canary("planted");
    assert_absent("the reply", &format!("body {planted} end"), &planted);
}

#[test]
fn the_failure_message_omits_the_secret() {
    let planted = canary("message");
    let text = format!("body {planted}");
    let caught = std::panic::catch_unwind(|| assert_absent("the reply", &text, &planted))
        .expect_err("the assertion must fail");
    let message = caught
        .downcast_ref::<String>()
        .cloned()
        .expect("a formatted panic message");
    assert!(!message.contains(&planted));
    assert!(message.contains("the reply"));
}
