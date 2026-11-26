// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the integration tests of `puddle-host` share. Test-only: `puddle-host` lists this crate
//! under `[dev-dependencies]` and no product crate depends on it. The helpers live in a library so
//! that every one is `pub` and none is dead in a test binary that uses a different subset.

#![forbid(unsafe_code)]
#![expect(
    clippy::missing_panics_doc,
    reason = "helpers fail the test that called them by panicking"
)]
#![expect(
    clippy::must_use_candidate,
    clippy::new_without_default,
    missing_docs,
    reason = "test helpers: the names and signatures carry the meaning, and a dropped result fails the test"
)]

pub mod support;
pub mod vm_support;
