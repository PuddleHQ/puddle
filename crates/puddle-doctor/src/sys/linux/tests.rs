// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn cpu_flags() {
    assert!(cpu_has_virtualization(
        "processor: 0\nflags\t\t: fpu vmx sse\n"
    ));
    assert!(cpu_has_virtualization("flags : svm"));
    assert!(!cpu_has_virtualization(
        "flags : fpu sse\nvmx: listed elsewhere\n"
    ));
    assert!(!cpu_has_virtualization(""));
}

#[test]
fn kvm_states() {
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        kvm_state(&dir.path().join("kvm")),
        HypervisorApi::NotInstalled { .. }
    ));
    let dev = dir.path().join("dev");
    std::fs::write(&dev, b"").unwrap();
    assert_eq!(kvm_state(&dev), HypervisorApi::Ready);
    assert!(matches!(
        kvm_state(dir.path()),
        HypervisorApi::QueryFailed { .. }
    ));
}

#[test]
fn the_probe_names_kvm_and_does_not_panic() {
    let _facts = hypervisor();
}
