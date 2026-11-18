// SPDX-License-Identifier: AGPL-3.0-or-later
//! The dormancy check: the v0.1 build, made without the `serve` and `mise` cargo features,
//! shows nothing of `puddle serve` or the mise layer. With a feature on, the same test says the
//! piece is offered, so a build that forgets a gate or a switch that leaks fails here.
//!
//! Run it as the shipped build is made: `scripts/check.sh dormant` (default features).
#![expect(
    clippy::expect_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::process::Command;

fn puddle(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_puddle"))
        .args(args)
        .output()
        .expect("the Puddle binary runs")
}

#[test]
fn the_serve_command_shows_only_when_it_is_built_in() {
    let on = cfg!(feature = "serve");
    let help = String::from_utf8(puddle(&["--help"]).stdout).unwrap();
    assert_eq!(help.contains("serve"), on, "--help: {help}");
    assert_eq!(help.contains("--headless"), on, "--help: {help}");
    let out = puddle(&["serve", "--port", "0"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    if on {
        assert_ne!(out.status.code(), Some(2), "{stderr}");
    } else {
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.contains("unknown argument: serve"), "{stderr}");
        let usage = stderr.split_once('\n').map_or("", |(_, rest)| rest);
        assert!(!usage.contains("serve"), "the usage names serve: {usage}");
    }
}

#[test]
fn the_mise_layer_stays_off_whatever_is_stored_unless_it_is_built_in() {
    let on = cfg!(feature = "mise");
    assert_eq!(puddle_settings::Tools::AVAILABLE, on);
    let mut global = puddle_settings::GlobalSettings::default();
    global.tools.mise = Some(true);
    assert_eq!(global.tools.mise(), on);
    let mode = puddle_settings::mise_mode(&global, None);
    assert_eq!(
        matches!(mode, puddle_settings::MiseMode::On { .. }),
        on,
        "{mode:?}"
    );
}
