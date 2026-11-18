// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-app` program: all logic is in the library.
#![forbid(unsafe_code)]
// No console window in release builds on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

#[expect(
    clippy::print_stderr,
    reason = "the process is ending: this is the one place a start-up failure can be reported"
)]
fn main() -> ExitCode {
    let options = puddle_app::Options::from_env();
    match puddle_app::run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let message = puddle_app::report_startup_failure(&options, &err);
            eprintln!("puddle-app: {message}");
            show_failure(&message);
            ExitCode::FAILURE
        }
    }
}

/// A release build on Windows has no console, so the reason goes in a message box.
#[cfg(all(windows, not(debug_assertions)))]
fn show_failure(message: &str) {
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Puddle")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

#[cfg(not(all(windows, not(debug_assertions))))]
fn show_failure(_message: &str) {}
