// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests for the certificate distribution point exception: a plain-HTTP `GET` or `HEAD`
//! of a CRL or authority-information-access URL named in a synced root is let through with no rule
//! and no toggle when the host is on the company or a private network, and nothing else about the
//! host is.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use puddle_netpolicy::PkiDistributionPoints;
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{AddressCheck, AddressVerdict, ProxyConfig};
use puddle_types::{
    BlockReason, CompanySignal, ConnectionDecision, ConnectionReason, LocalCategory, WorkspaceId,
};
use tokio::io::{AsyncWriteExt, BufReader};

use super::plain_keepalive::{answer, get, keep_alive_server};
use super::route::{LOCAL, Rig};

/// The guard of a workspace with every local toggle off, which still knows `LOCAL` as the
/// company network's address: nothing is allowed by it, and only a distribution point may use it.
struct CompanyLan {
    /// Addresses a distribution point may use.
    pki: Vec<IpAddr>,
}

impl AddressCheck for CompanyLan {
    fn check(&self, _workspace: &WorkspaceId, _addr: SocketAddr) -> AddressVerdict {
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Loopback))
    }

    fn company_signal(
        &self,
        _host: &puddle_types::Host,
        addr: SocketAddr,
    ) -> Option<CompanySignal> {
        self.pki
            .contains(&addr.ip())
            .then_some(CompanySignal::DnsSuffix)
    }

    fn pki_address(&self, _host: &puddle_types::Host, addr: SocketAddr) -> bool {
        self.pki.contains(&addr.ip())
    }
}

fn rig(web: SocketAddr, pki: &[IpAddr], addrs: &[IpAddr], urls: &[&str]) -> Rig {
    let points = PkiDistributionPoints::from_urls(
        urls.iter()
            .map(|u| u.replace("PORT", &web.port().to_string())),
    );
    Rig::with_proxy(
        &ProxyConfig::default(),
        StaticResolver::new().with("pki.test", addrs),
        Arc::new(CompanyLan { pki: pki.to_vec() }),
        |proxy| proxy.with_pki_distribution_points(points),
    )
}

const CRL: &str = "http://pki.test:PORT/crl/root.crl";

fn request(method: &str, web: SocketAddr, path: &str) -> String {
    get("pki.test", web, path).replacen("GET", method, 1)
}

#[tokio::test]
async fn a_listed_url_is_fetched_with_no_rule_and_no_toggle_and_the_record_says_why() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[LOCAL], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    let got = answer(&mut reader, "GET").await;
    assert_eq!(got.code, 200);
    assert_eq!(got.body, b"/crl/root.crl");
    reader
        .get_mut()
        .write_all(request("HEAD", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "HEAD").await.code, 200);
    assert_eq!(seen.lines(), ["GET /crl/root.crl", "HEAD /crl/root.crl"]);

    let events = rig.events(2).await;
    for event in &events {
        assert_eq!(event.decision, ConnectionDecision::Allow, "{event:?}");
        assert_eq!(
            event.reason,
            ConnectionReason::PkiDistributionPoint,
            "{event:?}"
        );
        assert_eq!(event.company_network, Some(CompanySignal::DnsSuffix));
    }
    assert_eq!(rig.policy.decisions(), 0, "no rule was asked");
    assert_eq!(rig.policy.pending().len(), 0, "nothing in the inbox");
}

#[tokio::test]
async fn a_method_other_than_get_or_head_is_decided_like_any_request() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[LOCAL], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let post = format!(
        "POST http://pki.test:{}/crl/root.crl HTTP/1.1\r\nHost: pki.test\r\ncontent-length: 0\r\n\r\n",
        web.port()
    );
    reader.get_mut().write_all(post.as_bytes()).await.unwrap();
    assert_eq!(answer(&mut reader, "POST").await.code, 403);
    assert_eq!(
        seen.lines(),
        Vec::<String>::new(),
        "nothing reached the server"
    );
    let events = rig.events(1).await;
    assert_ne!(events[0].reason, ConnectionReason::PkiDistributionPoint);
    assert_eq!(rig.policy.decisions(), 1, "the rules were asked");
}

#[tokio::test]
async fn another_path_of_the_same_host_is_decided_like_any_request() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[LOCAL], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 200);
    reader
        .get_mut()
        .write_all(request("GET", web, "/secret").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 403);
    assert_eq!(seen.lines(), ["GET /crl/root.crl"]);
    let events = rig.events(2).await;
    assert_eq!(events[0].reason, ConnectionReason::PkiDistributionPoint);
    assert_ne!(events[1].reason, ConnectionReason::PkiDistributionPoint);
}

#[tokio::test]
async fn a_port_the_url_does_not_name_is_decided_like_any_request() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[LOCAL], &["http://pki.test:1/crl/root.crl"]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 403);
    assert_eq!(seen.lines(), Vec::<String>::new());
}

#[tokio::test]
async fn an_address_the_exception_does_not_cover_is_decided_like_any_request() {
    let (web, seen) = keep_alive_server().await;
    // The guard says loopback is not a company or private address.
    let rig = rig(web, &[], &[LOCAL], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 403);
    assert_eq!(seen.lines(), Vec::<String>::new());
    let events = rig.events(1).await;
    assert_ne!(events[0].reason, ConnectionReason::PkiDistributionPoint);
}

#[tokio::test]
async fn a_name_that_also_resolves_to_an_address_outside_the_exception_is_not_let_through() {
    let (web, seen) = keep_alive_server().await;
    let other = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
    let rig = rig(web, &[LOCAL], &[LOCAL, other], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 403);
    assert_eq!(seen.lines(), Vec::<String>::new());
}

#[tokio::test]
async fn without_any_distribution_point_nothing_is_let_through() {
    let (web, _) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[LOCAL], &[]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 403);
}

#[tokio::test]
async fn a_name_that_does_not_resolve_is_left_to_the_ordinary_decision() {
    let (web, _) = keep_alive_server().await;
    let rig = rig(web, &[LOCAL], &[], &[CRL]);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(request("GET", web, "/crl/root.crl").as_bytes())
        .await
        .unwrap();
    let got = answer(&mut reader, "GET").await;
    assert_ne!(got.code, 200);
    let events = rig.events(1).await;
    assert_ne!(events[0].reason, ConnectionReason::PkiDistributionPoint);
}
