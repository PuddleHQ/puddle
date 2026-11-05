// SPDX-License-Identifier: AGPL-3.0-or-later
//! A destination behind a private-access client, against the real guard, rules and audit: the
//! intranet name resolves to a `100.64.0.0/10` address, the host reaches the intranet through the
//! client's tunnel, and a physical-LAN address sits next to it.
//!
//! These are the bars the company-network category must keep. Today the address is a
//! local/private network one, so the private toggle decides; the category adds its own toggle on
//! top of this and changes the refusal's name, not who may pass without a toggle or a rule.
//! Unix only: the guest agent end of the rig is.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::rig;

use std::net::IpAddr;

use puddle_e2e::lab::PrivateAccess;
use puddle_netpolicy::LocalAccess;
use puddle_proxy::testing::StaticResolver;
use puddle_types::LocalCategory;
use puddle_upstream::{Hop, ProxyAddr};
use rig::{Rig, allow, connection_records, rig_company, rig_guarded, try_tunnel, tunnel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const PUBLIC: &str = "93.184.215.14";

fn resolver() -> StaticResolver {
    let mut resolver = StaticResolver::new();
    for (name, ip) in PrivateAccess::answers() {
        resolver = resolver.with(name, &[ip]);
    }
    resolver.with("public.corp.test", &[PUBLIC.parse::<IpAddr>().unwrap()])
}

async fn rig_for(access: &PrivateAccess, local: LocalAccess) -> Rig {
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", access.tunnel_port()));
    rig_guarded(vec![hop], resolver(), local).await
}

fn authority(name: &str) -> String {
    format!("{name}:80")
}

/// A `GET /` through an open tunnel to `name`; the status line of the answer.
async fn get(mut reader: tokio::io::BufReader<tokio::net::TcpStream>, name: &str) -> String {
    reader
        .get_mut()
        .write_all(
            format!("GET / HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .unwrap();
    let mut answer = String::new();
    reader.read_to_string(&mut answer).await.unwrap();
    answer.lines().next().unwrap_or_default().to_owned()
}

#[tokio::test]
async fn with_the_private_toggle_off_the_intranet_name_is_refused_and_the_client_is_never_asked() {
    let access = PrivateAccess::start().await;
    let rig = rig_for(&access, LocalAccess::NONE).await;
    allow(&rig.store, PrivateAccess::NAME);

    let refusal = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(refusal.contains("403"), "{refusal}");
    assert!(
        refusal.contains("x-puddle-blocked: toggle:private"),
        "{refusal}"
    );
    assert!(
        access.carried().is_empty(),
        "the guard decides before any connection: {:?}",
        access.carried()
    );
    assert!(access.served().is_empty());
}

#[tokio::test]
async fn with_the_toggle_on_only_an_exact_allow_reaches_the_intranet() {
    let access = PrivateAccess::start().await;
    let local = LocalAccess::NONE.with_toggle(LocalCategory::Private, true);
    let rig = rig_for(&access, local).await;

    // No rule: pending, nothing carried.
    let pending = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(pending.contains("x-puddle-pending"), "{pending}");

    // A suffix rule alone does not reach a local address (R-14).
    allow(&rig.store, "*.corp.test");
    let wildcard = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(wildcard.contains("403"), "{wildcard}");
    assert!(access.carried().is_empty(), "{:?}", access.carried());

    // The exact name does.
    allow(&rig.store, PrivateAccess::NAME);
    let reader = tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME)).await;
    assert!(get(reader, PrivateAccess::NAME).await.contains("200"));
    assert_eq!(access.carried(), [authority(PrivateAccess::NAME)]);
    assert_eq!(access.served().len(), 1);
}

#[tokio::test]
async fn a_physical_lan_address_is_not_opened_by_what_opens_the_adapter() {
    let access = PrivateAccess::start().await;
    // Everything but the private toggle is on: the LAN address needs that one.
    let local = LocalAccess::NONE
        .with_toggle(LocalCategory::Loopback, true)
        .with_toggle(LocalCategory::LinkLocal, true);
    let rig = rig_for(&access, local).await;
    let lan = PrivateAccess::LAN_ADDRESS.to_string();
    allow(&rig.store, &lan);

    let refusal = try_tunnel(rig.agent.local_addr(), &authority(&lan))
        .await
        .unwrap_err();
    assert!(
        refusal.contains("x-puddle-blocked: toggle:private"),
        "{refusal}"
    );
    assert!(access.carried().is_empty(), "{:?}", access.carried());
}

#[tokio::test]
async fn on_a_proxied_route_the_check_runs_on_the_hosts_own_answer_and_the_audit_names_the_route() {
    let access = PrivateAccess::start().await;
    let rig = rig_for(&access, LocalAccess::NONE).await;
    allow(&rig.store, PrivateAccess::NAME);
    allow(&rig.store, "public.corp.test");

    // The intranet name is refused on the answer the host got, although the tunnel would carry it.
    let refusal = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(refusal.contains("toggle:private"), "{refusal}");

    // A name whose answer is public is not refused: the tunnel is asked (it has no such name, so
    // it connects to the name as it stands and answers 502, which proves it was asked).
    let public = try_tunnel(rig.agent.local_addr(), &authority("public.corp.test"))
        .await
        .unwrap_err();
    assert!(!public.contains("x-puddle-blocked: toggle"), "{public}");
    assert_eq!(access.carried(), [authority("public.corp.test")]);

    let records = connection_records(&rig.store, 2).await;
    let blocked = records
        .iter()
        .find(|r| r["host"] == PrivateAccess::NAME)
        .unwrap();
    assert_eq!(blocked["decision"], "blocked", "{blocked}");
    assert_eq!(blocked["reason"], "toggle:private", "{blocked}");
    assert_eq!(
        blocked["refused_address"],
        PrivateAccess::ADDRESS.to_string()
    );
    let carried = records
        .iter()
        .find(|r| r["host"] == "public.corp.test")
        .unwrap();
    // The tunnel was asked and refused (no such site behind it): the answer to a request that
    // got past the guard.
    assert_eq!(carried["decision"], "allow", "{carried}");
    assert_eq!(carried["outcome"], "upstream_refused", "{carried}");
}

/// The signals a host with the stand-in installed reads: the client's adapter and suffix, and the
/// physical LAN's subnet.
fn signals() -> puddle_netpolicy::StaticSignals {
    let adapter = PrivateAccess::adapter();
    puddle_netpolicy::StaticSignals::new()
        .with_private_access_client()
        .with_dns_suffix(adapter.dns_suffix)
        .with_vpn_route(IpAddr::V4(adapter.route.0), adapter.route.1)
        .with_lan_subnet("192.168.77.0".parse().unwrap(), 24)
}

async fn company_rig(access: &PrivateAccess, local: LocalAccess) -> Rig {
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", access.tunnel_port()));
    rig_company(vec![hop], resolver(), local, signals()).await
}

#[tokio::test]
async fn company_toggle_off_refuses_the_intranet_name_as_company_network() {
    let access = PrivateAccess::start().await;
    // The private toggle is on: it does not open a company-network address.
    let local = LocalAccess::NONE.with_toggle(LocalCategory::Private, true);
    let rig = company_rig(&access, local).await;
    allow(&rig.store, PrivateAccess::NAME);

    let refusal = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(refusal.contains("403"), "{refusal}");
    assert!(
        refusal.contains("x-puddle-blocked: company_network"),
        "{refusal}"
    );
    assert!(refusal.contains("Company network"), "{refusal}");
    assert!(access.carried().is_empty(), "{:?}", access.carried());
    let records = connection_records(&rig.store, 1).await;
    assert_eq!(
        records[0]["reason"], "company_network_off",
        "{}",
        records[0]
    );
    assert_eq!(records[0]["category"], "company_network", "{}", records[0]);
}

#[tokio::test]
async fn company_toggle_on_still_needs_an_exact_allow() {
    let access = PrivateAccess::start().await;
    let local = LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true);
    let rig = company_rig(&access, local).await;

    let pending = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(pending.contains("x-puddle-pending"), "{pending}");

    allow(&rig.store, "*.corp.test");
    let wildcard = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(wildcard.contains("403"), "{wildcard}");
    assert!(access.carried().is_empty(), "{:?}", access.carried());

    allow(&rig.store, PrivateAccess::NAME);
    let reader = tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME)).await;
    assert!(get(reader, PrivateAccess::NAME).await.contains("200"));
    let records = connection_records(&rig.store, 3).await;
    let allowed = records.last().unwrap();
    assert_eq!(allowed["decision"], "allow", "{allowed}");
    assert_eq!(allowed["category"], "company_network", "{allowed}");
}

#[tokio::test]
async fn a_physical_lan_address_stays_with_the_private_toggle_when_the_company_one_is_on() {
    let access = PrivateAccess::start().await;
    let local = LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true);
    let rig = company_rig(&access, local).await;
    let lan = PrivateAccess::LAN_ADDRESS.to_string();
    allow(&rig.store, &lan);

    let refusal = try_tunnel(rig.agent.local_addr(), &authority(&lan))
        .await
        .unwrap_err();
    assert!(
        refusal.contains("x-puddle-blocked: toggle:private"),
        "{refusal}"
    );
}

#[tokio::test]
async fn without_the_client_the_same_answer_is_plain_private() {
    let access = PrivateAccess::start().await;
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", access.tunnel_port()));
    // A host that reads no signals: the category is empty.
    let rig = rig_company(
        vec![hop],
        resolver(),
        LocalAccess::NONE,
        puddle_netpolicy::StaticSignals::new(),
    )
    .await;
    allow(&rig.store, PrivateAccess::NAME);
    let refusal = try_tunnel(rig.agent.local_addr(), &authority(PrivateAccess::NAME))
        .await
        .unwrap_err();
    assert!(
        refusal.contains("x-puddle-blocked: toggle:private"),
        "{refusal}"
    );
}
