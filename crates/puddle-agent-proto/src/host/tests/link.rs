// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest link, forwards and the SSH agent stream.

use super::*;

/// What a handler is told about the guest's link and ports.
#[derive(Default)]
pub(super) struct Reach {
    pub(super) link: std::sync::Mutex<Option<GuestLink>>,
    pub(super) ports: std::sync::Mutex<Vec<Vec<u16>>>,
}

struct Reaching(Arc<Reach>);

impl StreamHandler for Reaching {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.link.lock().unwrap() = Some(link);
    }

    async fn guest_ports(&self, ports: Vec<u16>) {
        self.0.ports.lock().unwrap().push(ports);
    }
}

#[tokio::test]
async fn the_handler_gets_the_link_at_hello_and_the_ports_clamped_to_what_a_port_can_be() {
    let reach = Arc::new(Reach::default());
    let mut h = start_handler(
        Arc::new(Reaching(Arc::clone(&reach))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    assert!(reach.link.lock().unwrap().is_none());
    send(&mut c, &AgentMessage::hello()).await;
    eventually("the handler gets the link at hello", WAIT, || {
        reach.link.lock().unwrap().is_some()
    })
    .await;
    // A hostile report: unsorted, 600 entries (the most a line holds), then repeats and a zero.
    let many: Vec<u16> = (10_001..=10_600).rev().collect();
    let line = format!(
        "{{\"type\":\"ports\",\"listening\":{}}}\n",
        serde_json::to_string(&many).unwrap()
    );
    assert!(line.len() < MAX_LINE);
    c.write_all(line.as_bytes()).await.unwrap();
    c.write_all(b"{\"type\":\"ports\",\"listening\":[8080,0,3000,8080]}\n")
        .await
        .unwrap();
    let told = eventually("the handler gets both port reports", WAIT, || {
        let ports = reach.ports.lock().unwrap();
        (ports.len() == 2).then(|| ports.clone())
    })
    .await;
    assert_eq!(told[0], (10_001..=10_512).collect::<Vec<u16>>());
    assert_eq!(told[1], [3000, 8080]);
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_forward_opened_through_the_link_is_the_connection_once_the_agent_says_ok() {
    use crate::forward::{Answer, OpenError, parse_request, read_line_exact};
    let reach = Arc::new(Reach::default());
    let mut h = start_handler(
        Arc::new(Reaching(Arc::clone(&reach))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::hello()).await;
    let link = eventually("the handler gets the link at hello", WAIT, || {
        reach.link.lock().unwrap().clone()
    })
    .await;
    // A fake agent: port 3000 echoes, anything else is refused.
    let mut inbound = std::mem::replace(&mut h.inbound, mpsc::unbounded_channel().1);
    tokio::spawn(async move {
        while let Some(mut stream) = inbound.recv().await {
            tokio::spawn(async move {
                let preamble = read_line_exact(&mut stream, MAX_PREAMBLE).await.unwrap();
                assert_eq!(parse_preamble(&preamble), Ok((StreamKind::Forward, 1)));
                let port = parse_request(&read_line_exact(&mut stream, 16).await.unwrap()).unwrap();
                if port != 3000 {
                    stream
                        .write_all(&Answer::refused("nothing listens").to_line())
                        .await
                        .unwrap();
                    return;
                }
                // The data follows the answer in the same write: it must not be lost.
                let mut reply = Answer::Ok.to_line();
                reply.extend_from_slice(b"hello");
                stream.write_all(&reply).await.unwrap();
                let mut buf = Vec::new();
                stream.read_to_end(&mut buf).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    let mut stream = link.open_forward(3000).await.unwrap();
    let mut hello = [0u8; 5];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(&hello, b"hello");
    stream.write_all(b"ping").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"ping");
    match link.open_forward(3001).await {
        Err(OpenError::Refused(reason)) => assert_eq!(reason, "nothing listens"),
        other => panic!("{other:?}"),
    }
    h.close().await.unwrap();
    assert!(matches!(
        link.open_forward(3000).await,
        Err(OpenError::Disconnected)
    ));
}

#[tokio::test]
async fn an_ssh_agent_stream_reaches_the_handler_with_every_byte_after_its_preamble() {
    let mut h = start();
    let mut s = h.control.open_stream().await.unwrap();
    // The preamble and the first request in one write: the bytes the host read ahead are not lost.
    let mut first = StreamKind::SshAgent.preamble().to_vec();
    first.extend_from_slice(b"\0\0\0\x01\x0b");
    s.write_all(&first).await.unwrap();
    s.write_all(b"more").await.unwrap();
    s.shutdown().await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(WAIT, s.read_to_end(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer, b"agent:\0\0\0\x01\x0bmore");
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_handler_without_an_ssh_agent_closes_its_streams() {
    let told = Arc::new(Told::default());
    let mut h = start_handler(Arc::new(Listening(told)), HostConfig::default());
    let mut s = h.control.open_stream().await.unwrap();
    s.write_all(StreamKind::SshAgent.preamble()).await.unwrap();
    s.write_all(b"\0\0\0\x01\x0b").await.unwrap();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(WAIT, s.read(&mut buf)).await.unwrap();
    assert!(matches!(read, Err(_) | Ok(0)), "{read:?}");
    h.close().await.unwrap();
}

/// Starts a host session, waits for its `GuestLink`, and runs a fake agent that answers the
/// editor stream's one request line with `answer`; the request it read goes down `seen`.
async fn editor_link_answered_with(
    answer: Vec<u8>,
) -> (GuestLink, mpsc::UnboundedReceiver<Vec<u8>>, Harness) {
    let reach = Arc::new(Reach::default());
    let mut h = start_handler(
        Arc::new(Reaching(Arc::clone(&reach))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::hello()).await;
    let link = eventually("the handler gets the link at hello", WAIT, || {
        reach.link.lock().unwrap().clone()
    })
    .await;
    let mut inbound = std::mem::replace(&mut h.inbound, mpsc::unbounded_channel().1);
    let (seen, requests) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let _control = c;
        while let Some(mut stream) = inbound.recv().await {
            let answer = answer.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let preamble = read_line_exact(&mut stream, MAX_PREAMBLE).await.unwrap();
                assert_eq!(parse_preamble(&preamble), Ok((StreamKind::Editor, 1)));
                let word = read_line_exact(&mut stream, 16).await.unwrap();
                seen.send(word).unwrap();
                stream.write_all(&answer).await.unwrap();
                let mut rest = Vec::new();
                let _ = stream.read_to_end(&mut rest).await;
            });
        }
    });
    (link, requests, h)
}

#[tokio::test]
async fn open_editor_sends_connect_and_hands_back_the_stream_on_an_ok_answer() {
    let (link, mut requests, _h) = editor_link_answered_with(Answer::Ok.to_line()).await;
    let stream = link.open_editor().await;
    assert!(stream.is_ok());
    assert_eq!(requests.recv().await.unwrap(), b"connect\n");
}

#[tokio::test]
async fn open_editor_reports_a_refusal_with_its_reason() {
    let (link, _requests, _h) =
        editor_link_answered_with(Answer::refused("no server yet").to_line()).await;
    let err = link.open_editor().await.unwrap_err();
    assert!(
        matches!(&err, OpenError::Refused(why) if why == "no server yet"),
        "{err}"
    );
}

#[tokio::test]
async fn editor_password_sends_password_and_returns_the_agents_answer() {
    let hex = "0123456789abcdef".repeat(2);
    let (link, mut requests, _h) =
        editor_link_answered_with(crate::editor_stream::password_line(&hex)).await;
    assert_eq!(link.editor_password().await.unwrap(), hex);
    assert_eq!(requests.recv().await.unwrap(), b"password\n");
}
