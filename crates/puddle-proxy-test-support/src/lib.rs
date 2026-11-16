// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rigs the `puddle-proxy` integration tests share: a proxy attacked by hostile guests ([`hostile`])
//! and the servers and raw client of the pull-proxy tests ([`pull`]). Test code only; never a
//! dependency of product crates.
#![forbid(unsafe_code)]
#![expect(
    clippy::missing_panics_doc,
    reason = "helpers fail the test that called them by panicking"
)]

pub mod hostile;
pub mod pull;
