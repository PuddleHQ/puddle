// SPDX-License-Identifier: AGPL-3.0-or-later
//! The soak run's sampler and judge. The sampler ([`sample`]) appends what a long run measures to
//! `metrics.tsv` ([`series`]); the judge ([`judge`]) reads that file at the end and returns the
//! findings the soak playbook's pass and fail lines define. [`run`] plays the playbook's day on
//! [`plan`]'s timeline against a real `puddle serve`. Tests and development code only.
// Locking a mutex that only this module's tasks hold cannot fail unless one of them panicked.
#![expect(
    clippy::unwrap_used,
    clippy::missing_panics_doc,
    reason = "development code: a poisoned lock is a bug to stop on"
)]

pub mod cli;
pub mod judge;
pub mod plan;
pub mod run;
pub mod sample;
pub mod series;
