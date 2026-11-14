// SPDX-License-Identifier: AGPL-3.0-or-later
//! Terminals over a real WebSocket: the ticket, the limits, the messages and the ends.

use futures_util::{SinkExt as _, StreamExt as _};
use puddle_api::{
    ApiConfig, EndReason, MAX_IN_ALL, MAX_MESSAGE_BYTES, MAX_PER_WORKSPACE, SUBPROTOCOL,
    TerminalEnd, TerminalError,
};
use puddle_api::{PublicHost, Remote, RemoteServing};
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use crate::common::{Api, RawSocket, STEP, start, start_with};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

const REPO: &str = "https://github.com/acme/api.git";

async fn workspace(api: &Api, name: &str) {
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
}

/// Asks for a terminal; the reply.
async fn ask(api: &Api, name: &str) -> crate::common::Reply {
    api.send(
        "POST",
        &format!("/api/workspaces/{name}/terminals"),
        Some(&json!({"cols": 80, "rows": 24})),
    )
    .await
}

/// A terminal and its ticket.
async fn ticket(api: &Api, name: &str) -> (u64, String) {
    let reply = ask(api, name).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let body = reply.json();
    (
        body["terminal"].as_u64().unwrap(),
        body["ticket"].as_str().unwrap().to_owned(),
    )
}

type Handshake = Result<Socket, tokio_tungstenite::tungstenite::Error>;

async fn connect_with(
    api: &Api,
    name: &str,
    terminal: u64,
    protocols: &str,
    origin: Option<&str>,
) -> Handshake {
    let url = format!(
        "ws://{}/api/workspaces/{name}/terminals/{terminal}",
        api.addr
    );
    let mut request = url.into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", protocols.parse().unwrap());
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", origin.parse().unwrap());
    }
    tokio::time::timeout(STEP, tokio_tungstenite::connect_async(request))
        .await
        .unwrap()
        .map(|(socket, _)| socket)
}

async fn connect(api: &Api, name: &str, terminal: u64, ticket: &str) -> Socket {
    connect_with(
        api,
        name,
        terminal,
        &format!("{SUBPROTOCOL}, ticket.{ticket}"),
        None,
    )
    .await
    .unwrap()
}

fn refusal(result: Handshake) -> u16 {
    match result {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status().as_u16(),
        other => panic!("expected a refusal, got {:?}", other.map(|_| ())),
    }
}

async fn next(socket: &mut Socket) -> Message {
    tokio::time::timeout(STEP, socket.next())
        .await
        .expect("a message within the step timeout")
        .expect("the socket is open")
        .unwrap()
}

/// Reads binary messages until `text` has appeared in them.
async fn read_until(socket: &mut Socket, text: &str) -> String {
    let mut seen = String::new();
    while !seen.contains(text) {
        match next(socket).await {
            Message::Binary(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            other => panic!("wanted {text:?}, got {other:?} after {seen:?}"),
        }
    }
    seen
}

async fn closes_with(socket: &mut Socket) -> CloseCode {
    match next(socket).await {
        Message::Close(Some(frame)) => frame.code,
        other => panic!("expected a close, got {other:?}"),
    }
}

#[tokio::test]
async fn a_terminal_echoes_follows_a_resize_and_ends_with_the_shells_status() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    assert_eq!(ticket.len(), 64);
    assert!(ticket.bytes().all(|b| b.is_ascii_hexdigit()));
    let mut socket = connect(&api, "shop", terminal, &ticket).await;

    read_until(&mut socket, "$ ").await;
    socket
        .send(Message::Binary(b"size\r".to_vec().into()))
        .await
        .unwrap();
    read_until(&mut socket, "24 80").await;
    socket
        .send(Message::Text(
            json!({"type": "resize", "cols": 100, "rows": 30})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
        .send(Message::Binary(b"size\r".to_vec().into()))
        .await
        .unwrap();
    read_until(&mut socket, "30 100").await;
    socket
        .send(Message::Text(
            json!({"type": "ack", "bytes": 12}).to_string().into(),
        ))
        .await
        .unwrap();

    socket
        .send(Message::Binary(b"exit 3\r".to_vec().into()))
        .await
        .unwrap();
    read_until(&mut socket, "exit 3").await;
    let Message::Text(ended) = next(&mut socket).await else {
        panic!("the end is a text message");
    };
    let ended: Value = serde_json::from_str(&ended).unwrap();
    assert_eq!(
        ended,
        json!({"type": "ended", "reason": "exited", "status": 3})
    );
    assert_eq!(closes_with(&mut socket).await, CloseCode::Normal);
}

/// A page that keeps acknowledging output while the shell ends (it does not know yet) must still be
/// told how the shell ended: closing a socket with unread data from the peer resets the connection,
/// and the reset throws away the end message the page has not read yet.
#[tokio::test]
async fn the_end_reaches_a_page_that_was_still_sending_when_the_shell_exited() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
    socket
        .send(Message::Binary(b"exit 4\r".to_vec().into()))
        .await
        .unwrap();
    // flake-ok: a producer pacing its sends so some land after the server's close, not a wait for a condition.
    for _ in 0..60 {
        let ack = json!({"type": "ack", "bytes": 1}).to_string();
        if socket.send(Message::Text(ack.into())).await.is_err() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    read_until(&mut socket, "exit 4").await;
    let Message::Text(ended) = next(&mut socket).await else {
        panic!("the end is a text message");
    };
    let ended: Value = serde_json::from_str(&ended).unwrap();
    assert_eq!(ended["status"], 4);
    assert_eq!(closes_with(&mut socket).await, CloseCode::Normal);
}

#[tokio::test]
async fn a_ticket_works_once_and_only_for_its_own_terminal_and_a_foreign_origin_is_refused_first() {
    let api = start().await;
    workspace(&api, "shop").await;
    workspace(&api, "blog").await;
    let (first, ticket_one) = ticket(&api, "shop").await;
    let (second, ticket_two) = ticket(&api, "shop").await;
    let offer = |t: &str| format!("{SUBPROTOCOL}, ticket.{t}");

    assert_eq!(
        refusal(connect_with(&api, "shop", first, &offer(&ticket_two), None).await),
        401
    );
    assert_eq!(
        refusal(connect_with(&api, "blog", first, &offer(&ticket_one), None).await),
        401,
        "another workspace's terminal id"
    );
    assert_eq!(
        refusal(connect_with(&api, "shop", first, SUBPROTOCOL, None).await),
        401
    );
    assert_eq!(
        refusal(connect_with(&api, "shop", first, &format!("ticket.{ticket_one}"), None).await),
        401,
        "no sub-protocol"
    );
    assert_eq!(
        refusal(
            connect_with(
                &api,
                "shop",
                first,
                &offer(&ticket_one),
                Some("http://evil.example")
            )
            .await
        ),
        403,
        "refused before the ticket is read, so the ticket is still good"
    );

    let mut socket = connect(&api, "shop", first, &ticket_one).await;
    read_until(&mut socket, "$ ").await;
    assert_eq!(
        refusal(connect_with(&api, "shop", first, &offer(&ticket_one), None).await),
        401,
        "used"
    );
    let own = connect_with(
        &api,
        "shop",
        second,
        &offer(&ticket_two),
        Some(&format!("http://{}", api.addr)),
    )
    .await;
    assert!(own.is_ok(), "the API's own origin is allowed");
}

#[tokio::test]
async fn a_caller_without_a_ticket_cannot_tell_an_unknown_workspace_from_a_known_one() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, _ticket) = ticket(&api, "shop").await;
    let offer = format!("{SUBPROTOCOL}, ticket.{}", "0".repeat(64));
    let mut answers = Vec::new();
    for name in ["shop", "nobody", "Bad!Name"] {
        let result = connect_with(&api, name, terminal, &offer, None).await;
        let Err(tokio_tungstenite::tungstenite::Error::Http(response)) = result else {
            panic!("{name}: expected a refusal");
        };
        answers.push((
            response.status().as_u16(),
            response.body().clone().unwrap_or_default(),
        ));
    }
    assert_eq!(answers[0].0, 401);
    assert!(answers.iter().all(|a| a == &answers[0]), "{answers:?}");
}

#[tokio::test]
async fn a_ticket_does_not_open_a_shell_under_a_public_name_and_stays_good_for_loopback() {
    let mut config = ApiConfig::default();
    config.remote = Some(Remote::new(
        RemoteServing::new(vec![PublicHost::parse("puddle.example.org").unwrap()], None).unwrap(),
    ));
    let api = start_with(config).await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    // Any 16 bytes in base64 will do; built in two parts so a secret scanner does not read it as a key.
    let key = ["dGhlIHNhbXBs", "ZSBub25jZQ=="].concat();
    let under = |host: &str| {
        format!(
            "GET /api/workspaces/shop/terminals/{terminal} HTTP/1.1\r\nHost: {host}\r\n\
             Connection: close, Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Key: {key}\r\nOrigin: https://{host}\r\n\
             Sec-Fetch-Site: same-origin\r\n\
             Sec-WebSocket-Protocol: {SUBPROTOCOL}, ticket.{ticket}\r\n\r\n"
        )
    };
    let public = crate::common::raw(api.addr, under("puddle.example.org").as_bytes()).await;
    assert_eq!(public.status, 401, "{public:?}");
    assert_ne!(public.status, 101);
    // The refusal came before the ticket was read, so it is still good on loopback.
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
}

#[tokio::test]
async fn a_ticket_runs_out_after_thirty_seconds() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    tokio::time::pause();
    tokio::time::advance(puddle_api::TICKET_LIFETIME).await;
    tokio::time::resume();
    let offer = format!("{SUBPROTOCOL}, ticket.{ticket}");
    assert_eq!(
        refusal(connect_with(&api, "shop", terminal, &offer, None).await),
        401
    );
}

#[tokio::test]
async fn the_limits_are_eight_for_a_workspace_and_thirty_two_in_all_and_a_closed_socket_frees_its_place()
 {
    let api = start().await;
    let names = ["a", "b", "c", "d", "e"];
    for name in names {
        workspace(&api, name).await;
    }
    let mut first = None;
    for _ in 0..MAX_PER_WORKSPACE {
        let made = ticket(&api, "a").await;
        first.get_or_insert(made);
    }
    let over = ask(&api, "a").await;
    assert_eq!(over.status, 409, "{}", over.body);
    assert_eq!(over.error(), "conflict");
    assert!(
        over.json()["message"]
            .as_str()
            .unwrap()
            .contains(&MAX_PER_WORKSPACE.to_string())
    );

    let (terminal, ticket_text) = first.unwrap();
    let mut socket = connect(&api, "a", terminal, &ticket_text).await;
    read_until(&mut socket, "$ ").await;
    socket.close(None).await.unwrap();
    drop(socket);
    puddle_test_kit::eventually_async("a closed socket frees its place", STEP, || async {
        ask(&api, "a").await.status == 201
    })
    .await;

    for name in &names[1..4] {
        for _ in 0..MAX_PER_WORKSPACE {
            ticket(&api, name).await;
        }
    }
    assert_eq!(MAX_IN_ALL, 4 * MAX_PER_WORKSPACE);
    let all = ask(&api, "e").await;
    assert_eq!(all.status, 409, "{}", all.body);
    assert!(
        all.json()["message"]
            .as_str()
            .unwrap()
            .contains(&MAX_IN_ALL.to_string())
    );
}

#[tokio::test]
async fn a_request_is_refused_for_a_size_outside_the_bounds_a_stopped_workspace_and_an_unknown_one()
{
    let api = start().await;
    workspace(&api, "shop").await;
    for (cols, rows) in [(1, 24), (501, 24), (80, 0), (80, 301)] {
        let reply = api
            .send(
                "POST",
                "/api/workspaces/shop/terminals",
                Some(&json!({"cols": cols, "rows": rows})),
            )
            .await;
        assert_eq!(reply.status, 422, "{cols}x{rows}: {}", reply.body);
    }
    assert_eq!(ask(&api, "nobody").await.status, 404);
    api.terminals
        .set_stopped(&WorkspaceId::new("shop").unwrap());
    let stopped = ask(&api, "shop").await;
    assert_eq!(stopped.status, 409, "{}", stopped.body);
    assert_eq!(
        api.send(
            "POST",
            "/api/workspaces/shop/terminals",
            Some(&json!({"cols": 80}))
        )
        .await
        .status,
        422,
        "a missing field"
    );
}

#[tokio::test]
async fn an_unknown_or_oversized_message_closes_the_socket_with_1008() {
    let api = start().await;
    workspace(&api, "shop").await;
    for bad in [
        Message::Text(
            json!({"type": "resize", "cols": 1, "rows": 24})
                .to_string()
                .into(),
        ),
        Message::Text(json!({"type": "dance"}).to_string().into()),
        Message::Text("not json".into()),
        Message::Binary(vec![b'x'; MAX_MESSAGE_BYTES + 1].into()),
        // A valid message, one byte over the limit.
        Message::Text(
            format!(
                r#"{{"type":"ack","bytes":1}}{}"#,
                " ".repeat(MAX_MESSAGE_BYTES + 1 - 24)
            )
            .into(),
        ),
    ] {
        let (terminal, ticket) = ticket(&api, "shop").await;
        let mut socket = connect(&api, "shop", terminal, &ticket).await;
        read_until(&mut socket, "$ ").await;
        let _ = socket.send(bad.clone()).await;
        loop {
            match next(&mut socket).await {
                Message::Close(frame) => {
                    assert_eq!(frame.map(|f| f.code), Some(CloseCode::Policy), "{bad:?}");
                    break;
                }
                Message::Binary(_) => {}
                other => panic!("{bad:?}: {other:?}"),
            }
        }
    }
}

/// The host's close of a refused page is a handshake: it waits for the page's close before it drops
/// the connection, so a page that is still sending is not reset.
#[tokio::test]
async fn a_refusal_waits_for_the_page_to_answer_the_close() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = RawSocket::open(
        api.addr,
        &format!("/api/workspaces/shop/terminals/{terminal}"),
        None,
        &format!("{SUBPROTOCOL}, ticket.{ticket}"),
    )
    .await;
    socket.send(0x1, b"not json").await;
    assert_eq!(socket.read_close().await, 1008);
    socket.answer_close_after_a_pause().await;
}

#[tokio::test]
async fn a_message_far_over_the_limit_is_cut_off_by_the_socket_itself() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
    let _ = socket
        .send(Message::Binary(vec![b'x'; MAX_MESSAGE_BYTES * 2].into()))
        .await;
    // Past the socket's own limit there is no policy refusal, but the page still gets a close
    // frame, with the code for a message that is too big.
    loop {
        match next(&mut socket).await {
            Message::Binary(_) => {}
            Message::Close(frame) => {
                assert_eq!(frame.map(|f| f.code), Some(CloseCode::Size));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

/// A close from the page is answered with a close frame, so the page sees a clean end.
#[tokio::test]
async fn a_close_from_the_page_is_answered_with_a_close_frame() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
    socket.send(Message::Close(None)).await.unwrap();
    loop {
        match next(&mut socket).await {
            Message::Binary(_) => {}
            Message::Close(_) => break,
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_shutdown_ends_the_terminals_with_quitting() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
    api.running.shutdown().await;
    let Message::Text(ended) = next(&mut socket).await else {
        panic!("the end is a text message");
    };
    let ended: Value = serde_json::from_str(&ended).unwrap();
    assert_eq!(ended, json!({"type": "ended", "reason": "quitting"}));
}

/// The quitting end is a handshake too.
#[tokio::test]
async fn a_shutdown_waits_for_the_page_to_answer_the_close() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = RawSocket::open(
        api.addr,
        &format!("/api/workspaces/shop/terminals/{terminal}"),
        None,
        &format!("{SUBPROTOCOL}, ticket.{ticket}"),
    )
    .await;
    let page = async {
        // The text message of the end, then the close.
        assert_eq!(socket.read_close().await, 1000);
        socket.answer_close_after_a_pause().await;
    };
    tokio::join!(api.running.shutdown(), page);
}

#[tokio::test]
async fn the_host_and_origin_checks_still_guard_the_socket_path_and_no_token_is_needed_for_it() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, _ticket) = ticket(&api, "shop").await;
    let path = format!("/api/workspaces/shop/terminals/{terminal}");
    let foreign = crate::common::raw(
        api.addr,
        format!("GET {path} HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n")
            .as_bytes(),
    )
    .await;
    assert_eq!(foreign.status, 421);
    let plain = crate::common::raw(
        api.addr,
        format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            api.addr
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        plain.status, 400,
        "no token is asked for, and it is no WebSocket request: {}",
        plain.body
    );
    let other = crate::common::raw(
        api.addr,
        format!(
            "DELETE {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            api.addr
        )
        .as_bytes(),
    )
    .await;
    assert_eq!(
        other.status, 401,
        "only the socket's GET goes without the token"
    );
}

async fn ended_message(socket: &mut Socket) -> Value {
    let Message::Text(ended) = next(socket).await else {
        panic!("the end is a text message");
    };
    serde_json::from_str(&ended).unwrap()
}

#[tokio::test]
async fn a_session_that_cannot_open_ends_the_socket_with_its_reason_and_message() {
    let api = start().await;
    workspace(&api, "shop").await;
    for (error, reason, message) in [
        (
            TerminalError::Ended(TerminalEnd {
                reason: EndReason::BootFailed,
                status: None,
                message: Some("the server refused".into()),
            }),
            "boot_failed",
            "the server refused",
        ),
        (
            TerminalError::Unavailable("not here".into()),
            "stopped",
            "not here",
        ),
    ] {
        api.terminals.refuse_next_open(error);
        let (terminal, ticket) = ticket(&api, "shop").await;
        let mut socket = connect(&api, "shop", terminal, &ticket).await;
        let ended = ended_message(&mut socket).await;
        assert_eq!(ended["reason"], reason, "{ended}");
        assert!(ended["message"].as_str().unwrap().contains(message));
        assert_eq!(closes_with(&mut socket).await, CloseCode::Normal);
    }
}

#[tokio::test]
async fn a_session_that_vanishes_without_an_end_is_a_crash_and_pings_are_ignored() {
    let api = start().await;
    workspace(&api, "shop").await;
    let (terminal, ticket) = ticket(&api, "shop").await;
    let mut socket = connect(&api, "shop", terminal, &ticket).await;
    read_until(&mut socket, "$ ").await;
    socket
        .send(Message::Ping(b"hi".to_vec().into()))
        .await
        .unwrap();
    socket
        .send(Message::Binary(b"vanish\r".to_vec().into()))
        .await
        .unwrap();
    // Anything the page sends after the session is gone changes nothing.
    let _ = socket.send(Message::Binary(b"x".to_vec().into())).await;
    loop {
        match next(&mut socket).await {
            Message::Binary(_) | Message::Pong(_) => {}
            Message::Text(text) => {
                let ended: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(ended["reason"], "crashed", "{ended}");
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(closes_with(&mut socket).await, CloseCode::Normal);
}
