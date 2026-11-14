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

#[tokio::test]
async fn carried_to_counts_the_named_host_and_leaves_the_hosts_own_downloads_out() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let access = PrivateAccess::start().await;
    for authority in [
        format!("{}:443", PrivateAccess::NAME),
        "background.invalid:443".to_owned(),
        format!("{}.extra.invalid:443", PrivateAccess::NAME),
    ] {
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", access.tunnel_port()))
            .await
            .unwrap();
        let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
        s.write_all(head.as_bytes()).await.unwrap();
        let mut answer = [0u8; 12];
        let _ = s.read(&mut answer).await.unwrap();
    }
    assert_eq!(access.carried().len(), 3, "{:?}", access.carried());
    assert_eq!(
        access.carried_to(PrivateAccess::NAME),
        [format!("{}:443", PrivateAccess::NAME)]
    );
}
