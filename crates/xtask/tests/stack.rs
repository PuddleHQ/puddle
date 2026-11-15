// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stack `xtask.exe` needs, against the stack it links.
//!
//! Windows gives a program 1 MiB of main-thread stack unless the link says otherwise, and an
//! overflow is an abort with no message. `build.rs` links 8 MiB; the heaviest work on the main
//! thread, the API schema build of `cargo xtask openapi`, must fit in half of that. Measured on
//! Windows (debug build): it needs between 896 and 960 KiB.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

/// The stack the program links on Windows (`build.rs`), in bytes.
fn linked_stack_bytes() -> usize {
    env!("PUDDLE_MAIN_STACK_BYTES").parse().unwrap()
}

#[test]
fn the_openapi_schema_build_fits_in_half_the_programs_main_thread_stack() {
    let json = std::thread::Builder::new()
        .stack_size(linked_stack_bytes() / 2)
        .spawn(puddle_api::openapi_json)
        .unwrap()
        .join()
        .unwrap();
    assert!(json.contains("\"openapi\""), "not an OpenAPI document");
}

/// A binary that lost the `/STACK` argument of `build.rs` would overflow with no message.
#[cfg(windows)]
#[test]
fn the_built_program_reserves_the_stack_that_build_rs_links() {
    let image = std::fs::read(env!("CARGO_BIN_EXE_xtask")).unwrap();
    assert_eq!(
        puddle_test_kit::pe_stack_reserve_bytes(&image),
        Some(linked_stack_bytes() as u64),
        "the /STACK link argument of crates/xtask/build.rs did not reach xtask.exe"
    );
}
