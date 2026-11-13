// SPDX-License-Identifier: GPL-3.0-or-later
//! Generates Tauri's context and the app ACL manifest .
//!
//! With a manifest, an app command is allowed to nobody until a capability grants it by name.
//! Without one, every app command is open to any local origin in any window. The shell has no
//! app commands today (the SPA's one Tauri call is the dialog plugin's `open`); `ping` is declared so the ACL tests have a
//! real command to be refused, and so the manifest is in force when the first real one arrives.

#![forbid(unsafe_code)]

use std::process::ExitCode;

/// The program's main-thread stack on Windows. The default is 1 MiB, which the host's start-up
/// (it runs inside Tauri's setup hook, on the main thread) outgrows in a debug build. The tests
/// read it as `PUDDLE_MAIN_STACK_BYTES` to check the start-up against the stack the program has.
const WINDOWS_MAIN_STACK_BYTES: u32 = 8 * 1024 * 1024;

/// `tauri-plugin-dialog` builds `rfd` against `TaskDialogIndirect`, which only comctl32 v6 has.
/// The program's manifest (Tauri's) asks for v6; a test executable has none and would not load
/// (0xc0000139), so the linker adds the dependency to the ones that link the plugin.
const COMMON_CONTROLS_V6: &str = "/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'";

fn main() -> ExitCode {
    if std::env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|env| env == "msvc") {
        println!("cargo::rustc-link-arg-bins=/STACK:{WINDOWS_MAIN_STACK_BYTES}");
        // Integration tests only: cargo has no such key for the library's own unit tests, which
        // stay free of the plugin instead (`shell::with_plugins`).
        println!("cargo::rustc-link-arg-tests={COMMON_CONTROLS_V6}");
        println!("cargo::rustc-link-arg-tests=/MANIFEST:EMBED");
    }
    println!("cargo::rustc-env=PUDDLE_MAIN_STACK_BYTES={WINDOWS_MAIN_STACK_BYTES}");
    match tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(&["ping"])),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            println!("cargo::error=puddle-app: tauri-build failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}
