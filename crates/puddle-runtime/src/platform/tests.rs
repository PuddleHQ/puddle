// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const ALL_OS: [HostOs; 3] = [HostOs::Windows, HostOs::Linux, HostOs::MacOs];

#[test]
fn the_table_per_os() {
    let w = HostOs::Windows.runtime_files();
    assert_eq!((w.msb, w.libkrunfw), ("msb.exe", "libkrunfw.dll"));
    assert!(w.firmware_beside_msb);
    let l = HostOs::Linux.runtime_files();
    assert_eq!((l.msb, l.libkrunfw), ("msb", "libkrunfw.so.5"));
    assert!(!l.firmware_beside_msb);
    let m = HostOs::MacOs.runtime_files();
    assert_eq!((m.msb, m.libkrunfw), ("msb", "libkrunfw.5.dylib"));
    assert!(!m.firmware_beside_msb);
}

#[test]
fn only_linux_names_its_firmware_by_version() {
    assert_eq!(
        HostOs::Linux.libkrunfw_versioned("5.6.1").as_deref(),
        Some("libkrunfw.so.5.6.1")
    );
    assert_eq!(HostOs::Windows.libkrunfw_versioned("5.6.1"), None);
    assert_eq!(HostOs::MacOs.libkrunfw_versioned("5.6.1"), None);
}

#[test]
fn every_installed_firmware_name_matches_its_prefix() {
    for os in ALL_OS {
        let f = os.runtime_files();
        assert!(f.libkrunfw.starts_with(f.libkrunfw_prefix), "{os:?}");
    }
}

#[test]
fn target_os_names_map_to_the_table() {
    for (name, os) in [
        ("windows", HostOs::Windows),
        ("linux", HostOs::Linux),
        ("macos", HostOs::MacOs),
    ] {
        assert_eq!(HostOs::from_target_os(name), Some(os));
    }
    assert_eq!(HostOs::from_target_os("freebsd"), None);
    assert_eq!(
        HostOs::from_target_os(std::env::consts::OS),
        Some(HostOs::current())
    );
}

#[test]
fn architectures_parse_and_name_their_agent_target() {
    for (spelling, arch) in [
        ("x86_64", GuestArch::X86_64),
        ("amd64", GuestArch::X86_64),
        ("aarch64", GuestArch::Aarch64),
        ("arm64", GuestArch::Aarch64),
    ] {
        assert_eq!(GuestArch::parse(spelling), Some(arch));
    }
    assert_eq!(GuestArch::parse("riscv64"), None);
    assert_eq!(
        GuestArch::X86_64.agent_target(),
        "x86_64-unknown-linux-musl"
    );
    assert_eq!(
        GuestArch::Aarch64.agent_target(),
        "aarch64-unknown-linux-musl"
    );
    assert_eq!(GuestArch::Aarch64.to_string(), "aarch64");
    assert!(GuestArch::X86_64.is_built());
    assert!(!GuestArch::Aarch64.is_built());
}

#[test]
fn the_build_script_knows_every_agent_target() {
    let script = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../ci/build-agent.sh"
    ))
    .unwrap();
    for arch in [GuestArch::X86_64, GuestArch::Aarch64] {
        assert!(script.contains(arch.agent_target()), "{arch}");
        assert!(script.contains(arch.name()), "{arch}");
    }
}
