// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

struct Fake(Vec<IfAddr>);

impl Interfaces for Fake {
    fn ipv4(&self) -> io::Result<Vec<IfAddr>> {
        Ok(self.0.clone())
    }
}

struct Broken;

impl Interfaces for Broken {
    fn ipv4(&self) -> io::Result<Vec<IfAddr>> {
        Err(io::Error::other("netlink says no"))
    }
}

fn iface(name: &str, index: u32, addr: [u8; 4]) -> IfAddr {
    IfAddr {
        name: name.to_owned(),
        index,
        addr: Ipv4Addr::from(addr),
    }
}

/// A fake `/sys/class/net` where `bridges` have a `bridge` directory and `others` don't.
fn sysfs(bridges: &[&str], others: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for name in bridges {
        std::fs::create_dir_all(dir.path().join(name).join("bridge")).unwrap();
    }
    for name in others {
        std::fs::create_dir_all(dir.path().join(name)).unwrap();
    }
    dir
}

const GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 17, 0, 1);

#[test]
fn a_bridge_that_owns_the_address_is_found_by_index() {
    let sys = sysfs(&["docker0"], &["eth0"]);
    let probe = SysProbe::new(
        Fake(vec![
            iface("eth0", 2, [10, 0, 2, 15]),
            iface("docker0", 5, [172, 17, 0, 1]),
        ]),
        sys.path(),
    );
    assert_eq!(probe.find(GATEWAY), Some(5));
}

#[test]
fn a_non_bridge_interface_with_the_address_is_not_a_bridge() {
    let sys = sysfs(&["docker0"], &["eth0", "veth1"]);
    let probe = SysProbe::new(
        Fake(vec![
            iface("eth0", 2, [172, 17, 0, 1]),
            iface("veth1", 9, [172, 17, 0, 1]),
        ]),
        sys.path(),
    );
    assert_eq!(probe.find(GATEWAY), None);
}

#[test]
fn a_bridge_without_the_address_is_not_found() {
    let sys = sysfs(&["docker0", "br-ab12"], &[]);
    let probe = SysProbe::new(
        Fake(vec![
            iface("docker0", 5, [172, 18, 0, 1]),
            iface("br-ab12", 6, [172, 19, 0, 1]),
        ]),
        sys.path(),
    );
    assert_eq!(probe.find(GATEWAY), None);
}

#[test]
fn a_failed_listing_means_no_bridge() {
    let sys = sysfs(&["docker0"], &[]);
    assert_eq!(SysProbe::new(Broken, sys.path()).find(GATEWAY), None);
}

#[test]
fn odd_interface_names_never_reach_the_filesystem() {
    let sys = sysfs(&[], &[]);
    let probe = SysProbe::new(
        Fake(vec![
            iface("", 1, [172, 17, 0, 1]),
            iface("..", 2, [172, 17, 0, 1]),
            iface(".", 3, [172, 17, 0, 1]),
            iface("a/b", 4, [172, 17, 0, 1]),
        ]),
        sys.path(),
    );
    assert_eq!(probe.find(GATEWAY), None);
}

#[cfg(target_os = "linux")]
#[test]
fn the_real_probe_finds_no_bridge_for_loopback() {
    // `lo` owns 127.0.0.1 but is not a bridge.
    assert_eq!(system_probe().find(Ipv4Addr::LOCALHOST), None);
}
