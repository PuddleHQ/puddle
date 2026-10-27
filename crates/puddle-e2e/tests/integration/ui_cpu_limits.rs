// SPDX-License-Identifier: GPL-3.0-or-later
//! The CPU limits the settings page offers are the host's: the API publishes no limits, so the
//! page keeps its own constants, and this test fails when they differ from `CpuCount`.
#![expect(clippy::panic, reason = "a test: a failed step fails it by panicking")]

use puddle_types::CpuCount;

const PAGE_LIMITS: &str = include_str!("../../../../ui/src/lib/settings/cpus.ts");

fn constant(name: &str) -> u32 {
    let prefix = format!("export const {name} = ");
    PAGE_LIMITS
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .and_then(|rest| rest.trim_end_matches(';').parse().ok())
        .unwrap_or_else(|| {
            panic!("`{name}` is not a number constant in ui/src/lib/settings/cpus.ts")
        })
}

#[test]
fn the_settings_page_offers_the_counts_the_host_accepts() {
    assert_eq!(constant("CPUS_MIN"), u32::from(CpuCount::MIN.get()));
    assert_eq!(constant("CPUS_MAX"), u32::from(CpuCount::MAX.get()));
    assert_eq!(constant("CPUS_DEFAULT"), u32::from(CpuCount::DEFAULT.get()));
}
