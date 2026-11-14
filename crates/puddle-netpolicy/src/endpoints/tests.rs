// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

struct Lan;
impl HostAddrs for Lan {
    fn is_own(&self, ip: IpAddr) -> bool {
        ip.is_loopback() || ip.is_unspecified() || ip == "192.168.1.10".parse::<IpAddr>().unwrap()
    }
}

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

#[test]
fn any_host_local_address_on_a_registered_port_matches() {
    let endpoints = PuddleEndpoints::new();
    let _api = endpoints.register(sa("127.0.0.1:7070"), EndpointKind::Api);
    for dest in [
        "127.0.0.1:7070",
        "127.0.0.2:7070",
        "[::1]:7070",
        "[::ffff:127.0.0.1]:7070",
        "0.0.0.0:7070",
        "[::]:7070",
        "192.168.1.10:7070",
    ] {
        assert_eq!(
            endpoints.find(sa(dest), &Lan),
            Some(EndpointKind::Api),
            "{dest}"
        );
    }
    for dest in ["192.168.1.11:7070", "127.0.0.1:7071", "8.8.8.8:7070"] {
        assert_eq!(endpoints.find(sa(dest), &Lan), None, "{dest}");
    }
}

#[test]
fn the_listener_s_own_address_matches_even_if_not_host_local() {
    struct Nothing;
    impl HostAddrs for Nothing {
        fn is_own(&self, _: IpAddr) -> bool {
            false
        }
    }
    let endpoints = PuddleEndpoints::new();
    let _a = endpoints.register(sa("10.9.9.9:9000"), EndpointKind::PinnedForward);
    let _b = endpoints.register(sa("10.9.9.8:9000"), EndpointKind::BrowserFront);
    assert_eq!(
        endpoints.find(sa("10.9.9.8:9000"), &Nothing),
        Some(EndpointKind::BrowserFront)
    );
    assert_eq!(endpoints.find(sa("10.9.9.7:9000"), &Nothing), None);
}

#[test]
fn dropping_the_registration_unregisters_only_that_listener() {
    let endpoints = PuddleEndpoints::new();
    let api = endpoints.register(sa("127.0.0.1:7070"), EndpointKind::Api);
    let pull = endpoints
        .clone()
        .register(sa("127.0.0.1:7071"), EndpointKind::PullProxy);
    assert_eq!(endpoints.list().len(), 2);
    drop(api);
    assert_eq!(
        endpoints.list(),
        vec![(sa("127.0.0.1:7071"), EndpointKind::PullProxy)]
    );
    drop(pull);
    assert_eq!(endpoints.list(), Vec::new());
}

#[test]
fn own_addresses_detects_loopback_and_unspecified() {
    for ip in ["127.0.0.1", "0.0.0.0", "::1", "::"] {
        assert!(OwnAddresses.is_own(ip.parse().unwrap()), "{ip}");
    }
    // TEST-NET-1 is never configured on a CI runner.
    assert!(!OwnAddresses.is_own("192.0.2.1".parse().unwrap()));
}

#[test]
fn loopback_and_unspecified_are_own_without_asking_the_host_to_bind() {
    let not_available = |_: IpAddr| Err(io::Error::from(io::ErrorKind::AddrNotAvailable));
    for ip in ["127.0.0.1", "0.0.0.0", "::1", "::"] {
        assert!(is_own_with(ip.parse().unwrap(), not_available), "{ip}");
    }
    assert!(!is_own_with("192.0.2.1".parse().unwrap(), not_available));
    assert!(is_own_with("192.0.2.1".parse().unwrap(), |_| Ok(())));
}

#[test]
fn kinds_describe_themselves() {
    for (kind, text) in [
        (EndpointKind::Api, "API"),
        (EndpointKind::PullProxy, "image-pull proxy"),
        (EndpointKind::BrowserFront, "browser front"),
        (EndpointKind::PinnedForward, "pinned port forward"),
        (EndpointKind::Other, "listener"),
    ] {
        assert_eq!(kind.to_string(), text);
    }
}

#[test]
fn a_successful_bind_or_a_failure_other_than_not_available_is_own() {
    assert!(bind_means_own(Ok(())));
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::Other,
        io::ErrorKind::AddrInUse,
        io::ErrorKind::OutOfMemory,
    ] {
        assert!(bind_means_own(Err(io::Error::from(kind))), "{kind:?}");
    }
}

#[test]
fn only_address_not_available_means_not_own() {
    assert!(!bind_means_own(Err(io::Error::from(
        io::ErrorKind::AddrNotAvailable
    ))));
}

#[test]
fn a_blocked_name_matches_exactly_or_with_its_subdomains_until_dropped() {
    let endpoints = PuddleEndpoints::new();
    let one = endpoints.block_name("Front.Example.org.", false, EndpointKind::ExternalFront);
    let tree = endpoints.block_name("apps.example.net", true, EndpointKind::Other);
    assert_eq!(
        endpoints.find_name("front.example.org"),
        Some(EndpointKind::ExternalFront)
    );
    assert_eq!(
        endpoints.find_name("FRONT.example.org."),
        Some(EndpointKind::ExternalFront)
    );
    assert_eq!(endpoints.find_name("a.front.example.org"), None);
    assert_eq!(
        endpoints.find_name("apps.example.net"),
        Some(EndpointKind::Other)
    );
    assert_eq!(
        endpoints.find_name("x.y.apps.example.net"),
        Some(EndpointKind::Other)
    );
    assert_eq!(endpoints.find_name("myapps.example.net"), None);
    assert_eq!(endpoints.find_name("example.net"), None);
    // A name block is not a listener: the address list stays empty.
    assert_eq!(endpoints.list(), []);
    drop(one);
    assert_eq!(endpoints.find_name("front.example.org"), None);
    assert_eq!(
        endpoints.find_name("apps.example.net"),
        Some(EndpointKind::Other)
    );
    drop(tree);
    assert_eq!(endpoints.find_name("apps.example.net"), None);
}

#[test]
fn the_new_kinds_describe_themselves() {
    assert_eq!(EndpointKind::RemoteApi.to_string(), "remote API listener");
    assert_eq!(
        EndpointKind::ExternalFront.to_string(),
        "reverse proxy in front of Puddle"
    );
}
