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

#[tokio::test]
async fn two_listeners_share_one_connection_budget() {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    let first = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let second = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let (first_addr, second_addr) = (first.local_addr().unwrap(), second.local_addr().unwrap());
    let open = Arc::new(Semaphore::new(1));
    let (stop, shutdown) = watch::channel(false);
    let tasks = [first, second].map(|listener| {
        tokio::spawn(serve(
            listener,
            Admission::Loopback,
            Dispatch {
                router: Router::new(),
                editor: None,
            },
            open.clone(),
            shutdown.clone(),
        ))
    });

    // A silent client holds the only slot (the header read timeout is far off).
    let holder = TcpStream::connect(first_addr).await.unwrap();
    puddle_test_kit::eventually(
        "the silent client holds the slot",
        Duration::from_secs(5),
        || open.available_permits() == 0,
    )
    .await;
    // The other listener refuses: its stream is closed without a byte.
    let mut refused = TcpStream::connect(second_addr).await.unwrap();
    let mut byte = [0_u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), refused.read(&mut byte))
        .await
        .expect("a refused connection is closed at once");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");

    drop(holder);
    stop.send(true).unwrap();
    for task in tasks {
        task.await.unwrap();
    }
}
