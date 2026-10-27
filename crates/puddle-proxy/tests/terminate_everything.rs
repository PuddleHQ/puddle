// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests of a workspace that decrypts every host: what is decrypted, what is passed through
//! untouched (a never-decrypt name, a host a tool refused the certificate for, anything that is
//! not TLS, a server Puddle's TLS cannot talk to), and that the switch applies to the next
//! connection.
mod terminate_support;

use std::sync::Arc;
use std::time::Duration;

use puddle_proxy::{InjectDecision, PassThrough};
use puddle_types::ConnectionDecision;
use rustls::pki_types::ServerName;
use terminate_support::{FakeServer, Flaw, Guest, Pki, Reply, RigBuilder, TestInjector, host};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// An injector for a workspace with no credential for the host: it adds nothing.
fn adds_nothing() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThrough)
}

async fn free_server(pki: &Pki, name: &str) -> FakeServer {
    FakeServer::tls(
        pki.server_config(name, Flaw::None),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await
}

#[tokio::test]
async fn a_host_with_no_credential_is_decrypted_when_the_workspace_decrypts_every_host() {
    let pki = Pki::new();
    let server = free_server(&pki, "free.test").await;
    let injector = adds_nothing();
    let rig = RigBuilder::new(&pki)
        .name("free.test", server.addr)
        .allow(vec!["bound.test", "free.test"])
        .injector(injector.clone())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("free.test:443", None).await.unwrap();
    let response = client.get("free.test", "/some/path?q=1").await;
    assert_eq!((response.status, response.text().as_str()), (200, "hello"));
    client.close().await;

    // The rules see the method and path of the request, and so does the audit (no query).
    assert_eq!(injector.paths(), ["/some/path?q=1"]);
    let events = rig.events(1).await;
    assert_eq!(events[0].reason.to_string(), "rule");
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
    let line = events[0].http.as_ref().expect("the request line");
    assert_eq!((line.method(), line.path()), ("GET", "/some/path"));
    assert_eq!(server.recorded().len(), 1);
}

#[tokio::test]
async fn with_the_switch_off_the_same_host_is_passed_through_and_the_tool_sees_the_real_certificate()
 {
    let pki = Pki::new();
    let server = free_server(&pki, "free.test").await;
    let injector = adds_nothing();
    let rig = RigBuilder::new(&pki)
        .name("free.test", server.addr)
        .allow(vec!["bound.test", "free.test"])
        .injector(injector.clone())
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest
        .tls_trusting("free.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    assert_eq!(client.get("free.test", "/").await.text(), "hello");
    client.close().await;
    assert_eq!(injector.calls(), 0, "nothing was decrypted");
    assert_eq!(rig.events(1).await[0].reason.to_string(), "rule");
}

#[tokio::test]
async fn a_name_on_the_never_decrypt_list_is_passed_through_and_says_so_in_the_audit() {
    let pki = Pki::new();
    let server = free_server(&pki, "bank.test").await;
    let sub = free_server(&pki, "api.bank.test").await;
    let injector = adds_nothing();
    let rig = RigBuilder::new(&pki)
        .name("bank.test", server.addr)
        .name("api.bank.test", sub.addr)
        .allow(vec!["bound.test", "bank.test", "api.bank.test"])
        .injector(injector.clone())
        .decrypting_all(true, &["bank.test"])
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest
        .tls_trusting("bank.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    assert_eq!(client.get("bank.test", "/").await.text(), "hello");
    client.close().await;
    assert_eq!(injector.calls(), 0);
    let events = rig.events(1).await;
    assert_eq!(events[0].reason.to_string(), "pass_through:never_decrypt");
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
    // Only the exact name is listed: its subdomain is decrypted.
    let mut guest = rig.guest().await;
    let mut client = guest.tls("api.bank.test:443", None).await.unwrap();
    assert_eq!(client.get("api.bank.test", "/").await.status, 200);
    client.close().await;
    assert_eq!(injector.calls(), 1);
}

#[tokio::test]
async fn a_host_with_a_credential_is_decrypted_even_when_the_never_decrypt_list_names_it() {
    let pki = Pki::new();
    let server = free_server(&pki, "bound.test").await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .decrypting_all(true, &["bound.test"])
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    assert_eq!(client.get("bound.test", "/").await.status, 200);
    assert!(
        server.recorded()[0].header("authorization").is_some(),
        "the credential was added"
    );
    client.close().await;
    assert_eq!(rig.events(1).await[0].reason.to_string(), "rule");
}

#[tokio::test]
async fn the_switch_applies_to_the_next_connection_without_a_restart() {
    let pki = Pki::new();
    let server = free_server(&pki, "free.test").await;
    let injector = adds_nothing();
    let rig = RigBuilder::new(&pki)
        .name("free.test", server.addr)
        .allow(vec!["bound.test", "free.test"])
        .injector(injector.clone())
        .build();
    let mut guest = rig.guest().await;
    let client = guest
        .tls_trusting("free.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    drop(client);
    assert_eq!(injector.calls(), 0);

    rig.set_decrypting_all(true, &[]);
    let mut client = guest.tls("free.test:443", None).await.unwrap();
    assert_eq!(client.get("free.test", "/").await.text(), "hello");
    client.close().await;
    assert_eq!(injector.calls(), 1);

    rig.set_decrypting_all(false, &[]);
    let client = guest
        .tls_trusting("free.test:443", std::slice::from_ref(&pki.root))
        .await;
    assert!(client.is_ok(), "passed through again");
}

#[tokio::test]
async fn a_tool_that_refuses_the_certificate_is_learned_and_the_host_is_passed_through_next_time() {
    let pki = Pki::new();
    let server = free_server(&pki, "pinned.test").await;
    let learned = Arc::new(PassThrough::with_limits(Duration::from_secs(3600), 8));
    let rig = RigBuilder::new(&pki)
        .name("pinned.test", server.addr)
        .allow(vec!["bound.test", "pinned.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .learned(Arc::clone(&learned))
        .build();
    // First connection: a tool whose roots do not hold the workspace CA refuses the certificate.
    let mut guest = rig.guest().await;
    let (port, _bridge) = guest.bridge("pinned.test:443").await;
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let config = Guest::client_config(std::slice::from_ref(&pki.root), &[]);
    let refused = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("pinned.test").unwrap(), tcp)
        .await;
    assert!(refused.is_err());
    let events = rig.events(1).await;
    assert_eq!(events[0].reason.to_string(), "guest_tls_rejected");
    assert!(
        learned.contains(&host("pinned.test")),
        "the host is remembered"
    );
    assert_eq!(learned.list().len(), 1);

    // The same tool, next time: the real server's certificate reaches it and it works.
    let mut client = guest
        .tls_trusting("pinned.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    assert_eq!(client.get("pinned.test", "/").await.text(), "hello");
    client.close().await;
    let events = rig.events(2).await;
    assert_eq!(events[1].reason.to_string(), "pass_through:learned");

    // Forgotten (or expired, which the list's own tests cover), the host is decrypted again.
    learned.forget(&host("pinned.test"));
    let mut client = guest.tls("pinned.test:443", None).await.unwrap();
    assert_eq!(client.get("pinned.test", "/").await.status, 200);
}

#[tokio::test]
async fn a_connection_that_is_not_tls_is_passed_through_with_the_bytes_already_read() {
    let pki = Pki::new();
    // A plain HTTP server on the TLS port: the tool speaks HTTP to it.
    let server = FakeServer::plain(Arc::new(|_| Reply::ok("plain"))).await;
    let rig = RigBuilder::new(&pki)
        .name("plain.test", server.addr)
        .allow(vec!["bound.test", "plain.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    let (code, mut stream) = guest.connect_to("plain.test:443").await;
    assert_eq!(code, 200);
    stream
        .write_all(b"GET /x HTTP/1.1\r\nHost: plain.test\r\n\r\n")
        .await
        .unwrap();
    let mut seen = vec![0_u8; 256];
    let n = stream.read(&mut seen).await.unwrap();
    assert!(String::from_utf8_lossy(&seen[..n]).contains("plain"));
    drop(stream);
    assert_eq!(server.recorded()[0].target, "/x");
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "pass_through:not_tls"
    );
}

#[tokio::test]
async fn a_protocol_where_the_server_speaks_first_is_passed_through_when_the_guest_stays_silent() {
    let pki = Pki::new();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            let _ = tcp.write_all(b"SSH-2.0-test\r\n").await;
        }
    });
    let rig = RigBuilder::new(&pki)
        .name("ssh.test", addr)
        .allow(vec!["bound.test", "ssh.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    let (code, mut stream) = guest.connect_to("ssh.test:443").await;
    assert_eq!(code, 200);
    let mut banner = vec![0_u8; 32];
    let n = tokio::time::timeout(Duration::from_secs(8), stream.read(&mut banner))
        .await
        .expect("the banner arrives after the short wait")
        .unwrap();
    assert_eq!(&banner[..n], b"SSH-2.0-test\r\n");
    drop(stream);
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "pass_through:not_tls"
    );
}

#[tokio::test]
async fn a_server_that_asks_for_a_client_certificate_is_passed_through() {
    let pki = Pki::new();
    let server = FakeServer::tls(
        pki.server_config_with("mutual.test", Flaw::None, true),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("mutual.test", server.addr)
        .allow(vec!["bound.test", "mutual.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest
        .tls_trusting("mutual.test:443", std::slice::from_ref(&pki.root))
        .await
        .unwrap();
    assert_eq!(client.get("mutual.test", "/").await.text(), "hello");
    client.close().await;
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "pass_through:server_client_certificate"
    );
}

#[tokio::test]
async fn a_server_whose_certificate_puddle_does_not_accept_is_passed_through_for_the_tool_to_judge()
{
    let pki = Pki::new();
    // A self-signed server: Puddle's verifier refuses it, a tool run with `curl -k` does not.
    let server = FakeServer::tls(
        pki.server_config("selfsigned.test", Flaw::SelfSigned),
        Arc::new(|_| Reply::ok("hello")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("selfsigned.test", server.addr)
        .allow(vec!["bound.test", "selfsigned.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    let (code, reader) = guest.connect_to("selfsigned.test:443").await;
    assert_eq!(code, 200);
    let tls = tokio_rustls::TlsConnector::from(Arc::new(terminate_support::trusting_anything()))
        .connect(ServerName::try_from("selfsigned.test").unwrap(), reader)
        .await
        .expect("the tool's own choice to trust it works");
    let mut client = terminate_support::Client {
        alpn: None,
        stream: tokio::io::BufReader::new(tls),
    };
    assert_eq!(client.get("selfsigned.test", "/").await.text(), "hello");
    client.close().await;
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "pass_through:server_certificate"
    );
}

#[tokio::test]
async fn a_client_that_asks_for_another_name_is_passed_through_not_refused() {
    let pki = Pki::new();
    let server = free_server(&pki, "free.test").await;
    let rig = RigBuilder::new(&pki)
        .name("free.test", server.addr)
        .allow(vec!["bound.test", "free.test"])
        .injector(adds_nothing())
        .decrypting_all(true, &[])
        .build();
    let mut guest = rig.guest().await;
    // The client names a host that is not the one in its CONNECT: with no credential at stake
    // the tool is left to the real server, as it is for any host that is not decrypted.
    let (code, reader) = guest.connect_to("free.test:443").await;
    assert_eq!(code, 200);
    let config = Guest::client_config(std::slice::from_ref(&pki.root), &[]);
    let refused = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("elsewhere.test").unwrap(), reader)
        .await;
    assert!(
        refused.is_err(),
        "the real server's certificate is not for that name"
    );
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "pass_through:sni_mismatch"
    );
}
