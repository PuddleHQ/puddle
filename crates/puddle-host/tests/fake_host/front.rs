// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor front: its port, forwarded ports and the servers it serves.

use super::*;

/// What [`window_gets`] gets, or `None` when nothing listens or the connection breaks.
async fn window_try(origin: &str) -> Option<String> {
    window_try_sends(origin, "GET", "/").await
}

fn stored_front_port(rig: &Rig) -> Option<u16> {
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_first_run_stores_the_port_its_front_took() {
    let rig = Rig::new();
    // A rig starts with a stored quiet port; this one is a data folder that never ran Puddle.
    std::fs::remove_file(HostPaths::new(rig.dir.path().join("data")).front_port_file()).unwrap();
    let host = rig.start().await;
    assert_eq!(
        stored_front_port(&rig),
        Some(front_port_of(&host)),
        "the port is stored"
    );
    host.shutdown().await;
}

/// The stored port is a quiet one (below the ephemeral ranges): a port the first run gave up is free
/// for anyone's port 0 or source port (the restart's own pull proxy binds one before the front does).
/// It is not held, since the front's plain bind cannot share a port on Windows.
#[tokio::test(flavor = "multi_thread")]
async fn the_front_keeps_its_port_across_restarts_of_the_host() {
    let rig = Rig::new();
    let port = puddle_test_kit::quiet_port();
    store_front_port(&rig, port);

    let first = rig.start().await;
    assert_eq!(front_port_of(&first), port);
    first.shutdown().await;
    drop(first); // the next run is a new process: its hold on the data folder ends

    // The same data folder, a new run: the same origin, so the editor's browser storage stays.
    let second = rig.start().await;
    assert_eq!(front_port_of(&second), port);
    second.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_front_port_falls_back_to_another_that_is_then_kept() {
    let rig = Rig::new();
    let (held, taken) = puddle_test_kit::bound_port();
    store_front_port(&rig, taken);

    let first = rig.start().await;
    let moved = front_port_of(&first);
    assert_ne!(moved, taken, "the held port cannot be the front's");
    let notice = first
        .problems()
        .list()
        .into_iter()
        .find(|p| p.key == "front-port-moved")
        .expect("the user is told the address moved");
    assert!(notice.detail.contains(&moved.to_string()), "{notice:?}");
    assert_eq!(
        stored_front_port(&rig),
        Some(moved),
        "the new port is stored"
    );
    first.shutdown().await;
    drop(first);

    // Next run the stored port is the usual one, with the old one still taken. It is a quiet port, as the
    // port the first run gave up is free for anyone's port 0.
    let quiet = puddle_test_kit::quiet_port();
    store_front_port(&rig, quiet);
    let second = rig.start().await;
    assert_eq!(front_port_of(&second), quiet);
    assert!(
        second
            .problems()
            .list()
            .iter()
            .all(|p| p.key != "front-port-moved"),
        "a port that is kept raises no notice"
    );
    second.shutdown().await;
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_front_port_that_is_not_a_port_is_replaced() {
    let rig = Rig::new();
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "not a port").unwrap();
    let host = rig.start().await;
    assert_eq!(stored_front_port(&rig), Some(front_port_of(&host)));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_front_port_that_cannot_be_stored_does_not_stop_the_host() {
    let rig = Rig::new();
    // A folder where the file belongs: it cannot be read as a port or replaced by one.
    let file = HostPaths::new(rig.dir.path().join("data")).front_port_file();
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir_all(&file).unwrap();
    let host = rig.start().await;
    let port = front_port_of(&host);
    assert_ne!(port, 0, "the front still runs");
    assert_eq!(stored_front_port(&rig), None);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_forwarded_port_s_origin_reaches_that_guest_port_through_the_agent() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;
    // An app of the workspace on its loopback (the agent stand-in shares this computer's).
    let app = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let app_port = app.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = app.accept().await {
            tokio::spawn(async move {
                let mut head = [0_u8; 1024];
                let _ = conn.read(&mut head).await;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 3\r\nconnection: close\r\n\r\napp",
                    )
                    .await;
                let _ = conn.shutdown().await;
            });
        }
    });
    let origin = browser_origin(&rig, &api, "acme").await;
    let forwarded = origin.replacen("http://", &format!("http://{app_port}."), 1);
    let answer = puddle_test_kit::eventually_async(
        "the front carries a request to the app's port",
        Duration::from_secs(20),
        || async {
            let answer = window_gets(&forwarded).await;
            answer.starts_with("HTTP/1.1 200").then_some(answer)
        },
    )
    .await;
    assert!(answer.ends_with("app"), "{answer}");
    assert_eq!(
        editor.accepted.load(Ordering::SeqCst),
        0,
        "the editor was not asked"
    );
    // The agent's own ports (its proxy) are refused by the agent, as for a forward: the
    // answer says so, where a free port would say that nothing listens.
    let proxy = window_gets(&origin.replacen("http://", "http://3128.", 1)).await;
    assert!(proxy.starts_with("HTTP/1.1 502"), "{proxy}");
    assert!(proxy.contains("belongs to Puddle's agent"), "{proxy}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_serves_the_server_the_workspace_was_started_with_not_what_the_settings_say_now()
{
    let rig = Rig::named_ids();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    let first = served_at(&origin).await;
    assert!(
        first.contains("puddle-secret-half="),
        "Microsoft's server gets the key cookie: {first}"
    );

    // The consent is withdrawn while the server runs: the running server is still Microsoft's.
    write_global_settings(&rig, &json!({"schema_version": 2}));
    let still = window_gets(&origin).await;
    assert!(
        still.contains("puddle-secret-half="),
        "the running server keeps its flavour: {still}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_front_serves_code_server_without_the_key_cookie_and_closes_with_the_host() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    let answer = served_at(&origin).await;
    assert!(!answer.contains("puddle-secret-half="), "{answer}");
    host.shutdown().await;
    // Not "the port refuses connections": the front's number can be another test's by now. The
    // same request that got the editor's 200 above (same name in `Host`) no longer does: a
    // stranger on the old port does not know the name.
    let after = window_try(&origin).await;
    assert!(
        after
            .as_deref()
            .is_none_or(|a| !a.starts_with("HTTP/1.1 200")),
        "the front's listener is gone after shutdown: {after:?}"
    );
}
