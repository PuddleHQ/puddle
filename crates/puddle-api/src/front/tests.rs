// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;
use std::sync::Arc;

use puddle_netpolicy::{AddressVerdict, LocalAccess, NetPolicy, Target};
use puddle_types::{BlockReason, LocalCategory, WorkspaceId};

use super::*;
use crate::remote::{NetworkEndpoint, NetworkListener, PublicHost, RemoteServing, TlsIdentity};

/// A resolver whose answers a test changes between refreshes.
#[derive(Default)]
struct Records(Mutex<HashMap<String, io::Result<Vec<IpAddr>>>>);

impl Records {
    fn set(&self, name: &str, ips: &[&str]) {
        let ips = ips.iter().map(|ip| ip.parse().unwrap()).collect();
        self.0.lock().unwrap().insert(name.to_owned(), Ok(ips));
    }

    fn fail(&self, name: &str) {
        let error = io::Error::other("no answer");
        self.0.lock().unwrap().insert(name.to_owned(), Err(error));
    }
}

impl NameResolver for Records {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            match self.0.lock().unwrap().get(name) {
                Some(Ok(ips)) => Ok(ips.clone()),
                Some(Err(e)) => Err(io::Error::new(e.kind(), e.to_string())),
                None => Err(io::Error::other("not found")),
            }
        })
    }
}

fn remote(hosts: &[&str], base: Option<&str>, proxies: &[&str]) -> Remote {
    let hosts = hosts
        .iter()
        .map(|h| PublicHost::parse(h).unwrap())
        .collect();
    let remote = Remote::new(RemoteServing::new(hosts, base).unwrap());
    if proxies.is_empty() {
        return remote;
    }
    let dir = tempfile::tempdir().unwrap();
    let cert = puddle_certs::devcert::listener::ensure(
        dir.path(),
        &["127.0.0.1".parse().unwrap()],
        &[],
        std::time::SystemTime::now(),
    )
    .unwrap();
    let identity = TlsIdentity::new(cert.cert_der().to_vec(), cert.key().pkcs8_der().to_vec());
    let allowed = proxies.iter().map(|p| p.parse().unwrap()).collect();
    let endpoint = NetworkEndpoint::new("127.0.0.1".parse().unwrap(), 0, allowed).unwrap();
    remote.with_network_listener(NetworkListener::new(endpoint, identity))
}

/// The guard with every local toggle on, so only the front's block can refuse.
fn guard(endpoints: &PuddleEndpoints) -> NetPolicy {
    let access = LocalAccess::NONE
        .with_toggle(LocalCategory::Private, true)
        .with_toggle(LocalCategory::Loopback, true)
        .with_toggle(LocalCategory::LinkLocal, true)
        .with_toggle(LocalCategory::CompanyNetwork, true);
    NetPolicy::new(Arc::new(access)).with_endpoints(endpoints.clone())
}

fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

fn dial(guard: &NetPolicy, name: Option<&str>, ip: &str, port: u16) -> AddressVerdict {
    let name = name.map(|n| puddle_types::Host::parse_normalised(n).unwrap());
    guard.check_address_named(
        &workspace(),
        name.as_ref(),
        SocketAddr::new(ip.parse().unwrap(), port),
    )
}

fn blocked(verdict: AddressVerdict) -> bool {
    verdict == AddressVerdict::Block(BlockReason::PuddleEndpoint)
}

#[tokio::test]
async fn an_alias_that_points_at_the_front_is_blocked_by_the_address_it_resolves_to() {
    let records = Arc::new(Records::default());
    records.set("puddle.example.org", &["192.168.1.50"]);
    records.set("ws.example.net", &["192.168.1.50"]);
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(&["puddle.example.org"], Some("ws.example.net"), &[]),
        &endpoints,
        records,
    );
    let guard = guard(&endpoints);
    assert!(
        !blocked(dial(
            &guard,
            Some("evil.attacker.test"),
            "192.168.1.50",
            443
        )),
        "nothing is blocked before the names are resolved"
    );
    front.refresh().await;
    // The guest's own name for the front: only the address gives it away.
    assert!(blocked(dial(
        &guard,
        Some("evil.attacker.test"),
        "192.168.1.50",
        443
    )));
    // The same machine as an IPv4-mapped IPv6 address.
    assert!(blocked(dial(&guard, None, "::ffff:192.168.1.50", 443)));
    // By the address itself, at the name stage.
    let target = Target::from_host(puddle_types::Host::parse_normalised("192.168.1.50").unwrap());
    assert_eq!(
        guard.check_target(&workspace(), &target, 443),
        Some(BlockReason::PuddleEndpoint)
    );
    // Only the front's port is blocked: that machine's other services are none of our business.
    assert!(!blocked(dial(&guard, None, "192.168.1.50", 22)));
    // Another machine on the same port is not the front.
    assert!(!blocked(dial(&guard, None, "192.168.1.51", 443)));
}

#[tokio::test]
async fn the_block_follows_the_ports_the_public_hosts_use() {
    let records = Arc::new(Records::default());
    records.set("puddle.example.org", &["10.1.2.3"]);
    records.set("alt.example.org", &["10.1.2.3"]);
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(&["puddle.example.org", "alt.example.org:8443"], None, &[]),
        &endpoints,
        records,
    );
    front.refresh().await;
    let guard = guard(&endpoints);
    assert!(blocked(dial(&guard, None, "10.1.2.3", 443)));
    assert!(blocked(dial(&guard, None, "10.1.2.3", 8443)));
    assert!(!blocked(dial(&guard, None, "10.1.2.3", 80)));
}

#[tokio::test]
async fn a_public_address_is_left_alone_because_other_sites_share_it() {
    let records = Arc::new(Records::default());
    records.set("puddle.example.org", &["8.8.8.8", "10.1.2.3"]);
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(&["puddle.example.org"], None, &[]),
        &endpoints,
        records,
    );
    front.refresh().await;
    let guard = guard(&endpoints);
    assert_eq!(dial(&guard, None, "8.8.8.8", 443), AddressVerdict::Allow);
    assert!(blocked(dial(&guard, None, "10.1.2.3", 443)));
}

#[tokio::test]
async fn the_admitted_proxies_and_a_public_host_written_as_an_address_are_blocked_without_a_lookup()
{
    let records = Arc::new(Records::default());
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(
            &["203.0.113.9", "[2001:db8::7]:8443"],
            Some("ws.example.net"),
            &["192.168.1.20", "2001:db8::20"],
        ),
        &endpoints,
        records,
    );
    let guard = guard(&endpoints);
    // Nothing was refreshed: these are the operator's own words.
    for (ip, port) in [
        ("203.0.113.9", 443),
        ("203.0.113.9", 8443),
        ("2001:db8::7", 443),
        ("2001:db8::7", 8443),
        ("192.168.1.20", 443),
        ("192.168.1.20", 8443),
        ("::ffff:192.168.1.20", 443),
        ("2001:db8::20", 443),
    ] {
        assert!(blocked(dial(&guard, None, ip, port)), "{ip}:{port}");
    }
    drop(front);
    assert!(
        !blocked(dial(&guard, None, "192.168.1.20", 443)),
        "released with the API"
    );
}

#[tokio::test]
async fn a_name_that_stops_resolving_keeps_its_last_addresses_and_a_moved_record_follows() {
    let records = Arc::new(Records::default());
    records.set("puddle.example.org", &["192.168.1.50"]);
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(&["puddle.example.org"], None, &[]),
        &endpoints,
        records.clone(),
    );
    let guard = guard(&endpoints);
    front.refresh().await;
    records.fail("puddle.example.org");
    front.refresh().await;
    assert!(
        blocked(dial(&guard, None, "192.168.1.50", 443)),
        "an outage opens nothing"
    );
    records.set("puddle.example.org", &["192.168.1.60"]);
    front.refresh().await;
    assert!(blocked(dial(&guard, None, "192.168.1.60", 443)));
    assert!(
        !blocked(dial(&guard, None, "192.168.1.50", 443)),
        "the old address is let go"
    );
}

#[tokio::test]
async fn a_name_that_never_answers_does_not_stop_the_others() {
    let records = Arc::new(Records::default());
    records.set("alt.example.org", &["10.9.9.9"]);
    let endpoints = PuddleEndpoints::new();
    let front = FrontAddresses::new(
        &remote(&["puddle.example.org", "alt.example.org"], None, &[]),
        &endpoints,
        records,
    );
    front.refresh().await;
    assert!(blocked(dial(&guard(&endpoints), None, "10.9.9.9", 443)));
}
