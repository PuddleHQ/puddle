// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle.exe` links the main-thread stack its start-up is checked against.
//!
//! Windows gives a program 1 MiB unless the link says otherwise, and `puddle serve` needs just
//! over that in a debug build (`serve_config.rs` runs the start-up on half of the linked size).
//! A binary that lost the `/STACK` argument of `build.rs` would overflow with no message.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]

#[test]
fn the_built_program_reserves_the_stack_that_build_rs_links() {
    let image = std::fs::read(env!("CARGO_BIN_EXE_puddle")).unwrap();
    assert_eq!(
        puddle_test_kit::pe_stack_reserve_bytes(&image),
        Some(env!("PUDDLE_MAIN_STACK_BYTES").parse().unwrap()),
        "the /STACK link argument of crates/puddle/build.rs did not reach puddle.exe"
    );
}
