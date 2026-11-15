// SPDX-License-Identifier: AGPL-3.0-or-later
//! The smallest count and the default the settings page assumes are the host's: the page keeps its
//! own constants, and this test fails when they differ from `CpuCount`. The upper bound is not a
//! constant: the API publishes it (the global maximum), so the page must not hard-code one.
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
fn the_settings_page_assumes_the_smallest_count_and_default_the_host_has() {
    assert_eq!(constant("CPUS_MIN"), u32::from(CpuCount::MIN.get()));
    assert_eq!(constant("CPUS_DEFAULT"), u32::from(CpuCount::DEFAULT.get()));
}

#[test]
fn the_settings_page_has_no_fixed_upper_bound() {
    assert!(
        !PAGE_LIMITS.contains("export const CPUS_MAX"),
        "the maximum comes from the host (`effective_limits.max_cpus`)"
    );
}
