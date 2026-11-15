// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn the_loopback_listener_admits_loopback_peers_only() {
    let admission = Admission::Loopback;
    assert!(admission.admits(ip("127.0.0.1")));
    assert!(admission.admits(ip("::1")));
    assert!(!admission.admits(ip("192.168.1.20")));
}

#[test]
fn the_network_listener_admits_the_listed_proxies_only_also_when_written_as_mapped_ipv6() {
    let identity = {
        let dir = tempfile::tempdir().unwrap();
        let cert = puddle_certs::devcert::listener::ensure(
            dir.path(),
            &[ip("127.0.0.1")],
            &[],
            std::time::SystemTime::now(),
        )
        .unwrap();
        crate::remote::TlsIdentity::new(cert.cert_der().to_vec(), cert.key().pkcs8_der().to_vec())
    };
    let admission = Admission::Proxies {
        allowed: vec![ip("192.168.1.20"), ip("2001:db8::7")],
        tls: tls_acceptor(&identity).unwrap(),
    };
    assert!(admission.admits(ip("192.168.1.20")));
    assert!(admission.admits(ip("::ffff:192.168.1.20")));
    assert!(admission.admits(ip("2001:db8::7")));
    assert!(!admission.admits(ip("192.168.1.21")));
    assert!(!admission.admits(ip("127.0.0.1")));
}

#[tokio::test]
async fn the_unclaimed_warning_runs_while_no_owner_exists() {
    use puddle_store::{Limits, ManualClock, Store};
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let secrets = Arc::new(store.take_account_secrets().unwrap());
    let sign_in = Arc::new(SignIn::new(store, secrets, clock, false).unwrap());
    assert!(sign_in.status().await.unwrap().setup_open);
    warn_while_unclaimed(&sign_in).await;
}

#[tokio::test]
async fn the_unclaimed_warning_is_left_out_once_the_owner_exists() {
    use puddle_store::{Limits, ManualClock, Store, UserName};
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let secrets = Arc::new(store.take_account_secrets().unwrap());
    secrets
        .create_owner(&UserName::new("Tijs").unwrap(), "$argon2id$x")
        .unwrap();
    let sign_in = Arc::new(SignIn::new(store, secrets, clock, false).unwrap());
    assert!(!sign_in.status().await.unwrap().setup_open);
    warn_while_unclaimed(&sign_in).await;
}
