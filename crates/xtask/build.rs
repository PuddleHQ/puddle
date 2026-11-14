// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gives `xtask.exe` the main-thread stack `cargo xtask openapi` needs on Windows.

#![forbid(unsafe_code)]

/// The program's main-thread stack on Windows. The default is 1 MiB, which `openapi`'s schema
/// build (`puddle_api::openapi_json`, on the main thread) nearly fills in a debug build; the other
/// programs link the same size. The tests read it as `PUDDLE_MAIN_STACK_BYTES` to check the work
/// against the stack the program has.
const WINDOWS_MAIN_STACK_BYTES: u32 = 8 * 1024 * 1024;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:{WINDOWS_MAIN_STACK_BYTES}");
    }
    println!("cargo::rustc-env=PUDDLE_MAIN_STACK_BYTES={WINDOWS_MAIN_STACK_BYTES}");
}
