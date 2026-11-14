// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_intranet_answer_and_the_route_are_inside_cgnat_and_the_lan_host_is_not() {
    let adapter = PrivateAccess::adapter();
    let inside = |ip: Ipv4Addr| {
        let (net, bits) = adapter.route;
        u32::from(ip) >> (32 - u32::from(bits)) == u32::from(net) >> (32 - u32::from(bits))
    };
    assert!(inside(PrivateAccess::ADDRESS));
    assert!(inside(adapter.address));
    assert!(!inside(PrivateAccess::LAN_ADDRESS));
    assert!(PrivateAccess::NAME.ends_with(adapter.dns_suffix));
}

#[tokio::test]
async fn the_root_is_the_one_that_signed_the_intranet_sites_certificate() {
    let access = PrivateAccess::start().await;
    assert_eq!(access.root(), access.web.root());
}
