// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn defaults_are_valid() {
    let c = WorkspaceConfig::default();
    assert_eq!(c.default_size, DiskSize::gib(32));
    assert_eq!(
        c.maintenance_image.as_str(),
        WorkspaceConfig::DEFAULT_MAINTENANCE_IMAGE
    );
    assert_eq!(c.maintenance_memory.get(), 1024);
}

#[test]
fn maintenance_names_fit_every_workspace_id() {
    let longest = WorkspaceId::new(&"w".repeat(WorkspaceId::MAX_LEN)).unwrap();
    let name = maintenance_name(&longest).unwrap();
    assert!(is_maintenance_name(name.as_str()));
    assert!(is_maintenance_name("m--acme"));
    assert!(!is_maintenance_name("acme"));
    assert!(!is_maintenance_name("m--"));
    assert!(!is_maintenance_name("m---a"));
}
