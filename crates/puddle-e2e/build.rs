// SPDX-License-Identifier: GPL-3.0-or-later
//! Gives the UI fixture backend the main-thread stack its start-up needs on Windows.

#![forbid(unsafe_code)]

/// The program's main-thread stack on Windows. The default is 1 MiB, which the fixture's start-up
/// (the real API router's schema builders on the main thread, inside `#[tokio::main]`) outgrows in
/// a debug build; `puddle-app` links the same size. `ui_fixture::stack` and the tests read it as
/// `PUDDLE_MAIN_STACK_BYTES` to check the start-up against the stack the program has.
const WINDOWS_MAIN_STACK_BYTES: u32 = 8 * 1024 * 1024;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:{WINDOWS_MAIN_STACK_BYTES}");
    }
    println!("cargo::rustc-env=PUDDLE_MAIN_STACK_BYTES={WINDOWS_MAIN_STACK_BYTES}");
}
