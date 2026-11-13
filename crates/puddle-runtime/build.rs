// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads the msb runtime version this build of Puddle runs from the workspace `Cargo.lock`: the
//! version of the `microsandbox` SDK package, which the root `Cargo.toml` takes from the fork tag
//! `v<version>` (the SDK and the bundled runtime move together, so the tag there is the only
//! place the version is written). Fails the build when the SDK doesn't come from that fork tag, or
//! when the SDK's crates resolve to more than one source (a half-done bump).
//!
//! It also reads `ci/msb-runtime.sha256` and embeds the SHA-256 of the msb binary and the firmware
//! library of that version for the target (empty where the list has none), which the runtime
//! check compares the installed files against at start-up.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[path = "build/built_for.rs"]
mod built_for;
#[path = "build/digests.rs"]
mod digests;
#[path = "build/pin_list.rs"]
mod pin_list;

use built_for::{built_for, parts};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            println!("cargo::error=puddle-runtime: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let lock_path = Path::new(&manifest_dir).join("../../Cargo.lock");
    println!("cargo::rerun-if-changed={}", lock_path.display());
    let lock = std::fs::read_to_string(&lock_path).map_err(|e| {
        format!(
            "cannot read the workspace lock file {}: {e}",
            lock_path.display()
        )
    })?;
    let version = built_for(&lock)?;
    let [major, minor, patch, puddle] = parts(&version)?;
    println!("cargo::rustc-env=PUDDLE_MSB_BUILT_FOR={version}");
    let out_dir = std::env::var_os("OUT_DIR").ok_or("cargo gave no OUT_DIR")?;
    let generated = PathBuf::from(out_dir).join("built_for_version.rs");
    std::fs::write(
        &generated,
        format!("RuntimeVersion::from_parts({major}, {minor}, {patch}, {puddle})\n"),
    )
    .map_err(|e| format!("cannot write {}: {e}", generated.display()))?;
    embed_digests(&manifest_dir, &version)
}

/// The pinned SHA-256 of the msb binary and the firmware library for this target, as env vars.
fn embed_digests(manifest_dir: &str, version: &str) -> Result<(), String> {
    let pins_path = Path::new(manifest_dir).join("../../ci/msb-runtime.sha256");
    println!("cargo::rerun-if-changed={}", pins_path.display());
    let pins = std::fs::read_to_string(&pins_path).map_err(|e| {
        format!(
            "cannot read the runtime pin list {}: {e}",
            pins_path.display()
        )
    })?;
    let target = |key: &str| std::env::var(key).unwrap_or_default();
    let expected = digests::expected(
        &pins,
        version,
        &target("CARGO_CFG_TARGET_OS"),
        &target("CARGO_CFG_TARGET_ARCH"),
    )?;
    for (name, value) in [
        ("PUDDLE_MSB_SHA256", expected.msb),
        ("PUDDLE_FIRMWARE_SHA256", expected.firmware),
        ("PUDDLE_FIRMWARE_VERSION", expected.firmware_version),
    ] {
        println!("cargo::rustc-env={name}={}", value.unwrap_or_default());
    }
    Ok(())
}
