// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which `*.` bases cover names anyone can register.
//!
//! `psl::domain(base).is_none()` catches a base that is itself a public suffix (`*.co.uk`). It
//! misses a registrable base with public suffixes below it: `amazonaws.com` is registrable, yet
//! `s3.amazonaws.com` is a public suffix, so a guest that owns `x.s3.amazonaws.com` would match
//! `*.amazonaws.com`. The `psl` crate answers lookups only; it cannot list rules. So
//! `public_suffix_parents.txt` holds every name that is a proper parent (two labels or more) of
//! a rule in the list, and a test compares it with the list the `psl` crate was built from.

use std::collections::HashSet;
use std::sync::LazyLock;

static PARENTS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| include_str!("public_suffix_parents.txt").lines().collect());

/// Whether a `*.base` pattern would cover names anyone can register: `base` is a public suffix,
/// or one of the public suffixes lies below it.
pub(super) fn covers_public_suffix(base: &str) -> bool {
    psl::domain(base.as_bytes()).is_none() || PARENTS.contains(base)
}

#[cfg(test)]
mod tests;
