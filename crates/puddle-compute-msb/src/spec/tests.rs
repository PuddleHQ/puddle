// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;

use super::*;

/// The `SandboxBuilder` methods [`builder`] calls, read from this file's source.
fn methods_called_by_builder() -> BTreeSet<String> {
    let source = include_str!("../spec.rs");
    let start = source.find("pub fn builder(").unwrap();
    let body = &source[start..];
    let end = body.find("\n}\n").unwrap();
    let body = &body[..end];
    let mut called = BTreeSet::new();
    for (i, _) in body.match_indices('.') {
        let rest = &body[i + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() && rest[name.len()..].starts_with('(') {
            called.insert(name);
        }
    }
    // Calls on Puddle's own types and on the nested mount/volume builders.
    for ours in [
        "get",
        "as_str",
        "iter",
        "as_mib",
        "bind",
        "readonly",
        "named",
        "named_with",
        "ensure_exists",
        "disk",
        "size",
        "owned_with",
    ] {
        called.remove(ours);
    }
    called
}

fn listed(setting: fn(&SdkSetting) -> bool) -> BTreeSet<String> {
    SDK_OPTIONS
        .iter()
        .filter(|o| setting(&o.setting))
        .filter_map(|o| o.option.strip_prefix("SandboxBuilder::"))
        .flat_map(|names| names.split(" / ").map(str::to_owned))
        .collect()
}

#[test]
fn the_option_list_matches_what_the_builder_sets() {
    let called = methods_called_by_builder();
    let set = listed(|s| matches!(s, SdkSetting::Set(_)));
    assert_eq!(
        called, set,
        "SDK_OPTIONS `Set` entries differ from builder()"
    );
    let defaulted = listed(|s| matches!(s, SdkSetting::Default(_)));
    assert!(
        set.is_disjoint(&defaulted),
        "an option is both set and default"
    );
}

#[test]
fn max_memory_is_only_ever_pinned_to_the_memory_size() {
    // Creation never sets it (the SDK then makes it equal to memory); the only call is in
    // set_memory, with the same value as memory.
    let spec_source = include_str!("../spec.rs");
    let builder_fn = &spec_source[spec_source.find("pub fn builder(").unwrap()..];
    assert!(!builder_fn[..builder_fn.find("\n}\n").unwrap()].contains("max_memory"));
    let call = format!(".{}(", "max_memory");
    for (file, source) in [
        ("lib.rs", include_str!("../lib.rs")),
        ("config.rs", include_str!("../config.rs")),
        ("error.rs", include_str!("../error.rs")),
        ("image.rs", include_str!("../image.rs")),
        ("sandbox.rs", include_str!("../sandbox.rs")),
        ("volume.rs", include_str!("../volume.rs")),
    ] {
        assert!(!source.contains(&call), "{file} sets max-memory");
    }
    let runtime = include_str!("../runtime.rs");
    assert_eq!(runtime.matches(&call).count(), 1);
    assert!(runtime.contains(&format!(".memory(mib){call}mib)")));
}

#[test]
fn max_cpus_is_only_ever_pinned_to_the_cpu_count() {
    let spec_source = include_str!("../spec.rs");
    let builder_fn = &spec_source[spec_source.find("pub fn builder(").unwrap()..];
    assert!(!builder_fn[..builder_fn.find("\n}\n").unwrap()].contains("max_cpus"));
    let call = format!(".{}(", "max_cpus");
    for (file, source) in [
        ("lib.rs", include_str!("../lib.rs")),
        ("config.rs", include_str!("../config.rs")),
        ("error.rs", include_str!("../error.rs")),
        ("image.rs", include_str!("../image.rs")),
        ("sandbox.rs", include_str!("../sandbox.rs")),
        ("volume.rs", include_str!("../volume.rs")),
    ] {
        assert!(!source.contains(&call), "{file} sets max-cpus");
    }
    let runtime = include_str!("../runtime.rs");
    assert_eq!(runtime.matches(&call).count(), 1);
    assert!(runtime.contains(&format!(".cpus(count){call}count)")));
}

fn spec() -> SandboxSpec {
    SandboxSpec::new(
        puddle_types::SandboxName::new("rootdisk").unwrap(),
        puddle_types::ImageRef::new("alpine").unwrap(),
    )
}

#[tokio::test]
async fn a_spec_with_a_root_disk_makes_a_config_with_that_managed_size() {
    let config =
        Box::pin(builder(&spec().with_root_disk(puddle_compute::DiskSize::gib(16))).build())
            .await
            .unwrap();
    assert_eq!(
        config.spec.image.oci_managed_root_disk_size_mib(),
        Some(16_384)
    );
}

#[tokio::test]
async fn a_spec_without_one_leaves_the_runtimes_default() {
    let config = Box::pin(builder(&spec()).build()).await.unwrap();
    assert_eq!(config.spec.image.oci_managed_root_disk_size_mib(), None);
}

#[test]
fn every_option_says_why() {
    let mut seen = BTreeSet::new();
    for o in SDK_OPTIONS {
        assert!(!o.why.is_empty(), "{}", o.option);
        assert!(seen.insert(o.option), "{} listed twice", o.option);
        match o.setting {
            SdkSetting::Set(v) | SdkSetting::Default(v) => assert_ne!(v, ""),
        }
    }
}
