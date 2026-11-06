// SPDX-License-Identifier: GPL-3.0-or-later
//! The session against the fake runtime's SSH server with its pseudo-terminal stand-in.

use std::time::Duration;

use puddle_compute::fake::pty;

use super::*;

const STEP: Duration = Duration::from_secs(10);

fn workspace() -> WorkspaceId {
    WorkspaceId::new("shop").unwrap()
}

fn identity() -> Identity {
    let dir = tempfile::tempdir().unwrap();
    let key = crate::ssh_login::LoginKey::open(dir.path()).unwrap();
    Identity::load(key.private_path()).unwrap()
}

async fn session(hub: &Hub, user: &str) -> TerminalLink {
    let size = TerminalSize::new(80, 24).unwrap();
    open(
        hub,
        Open {
            workspace: &workspace(),
            identity: &identity(),
            user,
            size,
        },
        |stream| async move { pty::serve(stream).await },
        || EndReason::Crashed,
    )
    .await
    .unwrap()
}

async fn bytes_until(link: &mut TerminalLink, text: &str) -> String {
    let mut seen = String::new();
    while !seen.contains(text) {
        match tokio::time::timeout(STEP, link.output.recv())
            .await
            .unwrap()
        {
            Some(TerminalOutput::Bytes(b)) => seen.push_str(&String::from_utf8_lossy(&b)),
            other => panic!("wanted {text:?}, got {other:?} after {seen:?}"),
        }
    }
    seen
}

async fn type_line(link: &TerminalLink, line: &str) {
    link.input
        .send(TerminalInput::Bytes(format!("{line}\r").into_bytes()))
        .await
        .unwrap();
}

async fn the_end(link: &mut TerminalLink) -> TerminalEnd {
    loop {
        match tokio::time::timeout(STEP, link.output.recv())
            .await
            .unwrap()
        {
            Some(TerminalOutput::Ended(end)) => return end,
            Some(TerminalOutput::Bytes(_)) => {}
            None => panic!("the link closed without an end"),
        }
    }
}

#[tokio::test]
async fn the_shell_runs_as_the_session_user_has_the_pty_the_page_asked_for_and_follows_a_resize() {
    let hub = Hub::default();
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "whoami").await;
    bytes_until(&mut link, "agent\r\n").await;
    type_line(&link, "term").await;
    bytes_until(&mut link, "xterm-256color").await;
    type_line(&link, "size").await;
    bytes_until(&mut link, "24 80").await;
    link.input
        .send(TerminalInput::Resize(TerminalSize::new(120, 40).unwrap()))
        .await
        .unwrap();
    type_line(&link, "size").await;
    bytes_until(&mut link, "40 120").await;
}

#[tokio::test]
async fn the_shells_exit_status_and_its_last_output_reach_the_page() {
    let hub = Hub::default();
    let mut link = session(&hub, "root").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "exit 3").await;
    let seen = bytes_until(&mut link, "exit 3").await;
    assert!(seen.contains("exit 3"));
    let end = the_end(&mut link).await;
    assert_eq!(
        end,
        TerminalEnd {
            reason: EndReason::Exited,
            status: Some(3),
            message: None
        }
    );
}

#[tokio::test]
async fn a_flood_stops_at_one_mebibyte_unacknowledged_and_goes_on_when_acknowledged() {
    let hub = Hub::default();
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "flood").await;
    let mut received: u64 = 0;
    // Everything the host sends until it goes quiet, with no acknowledgement.
    while let Ok(Some(TerminalOutput::Bytes(b))) =
        tokio::time::timeout(Duration::from_millis(700), link.output.recv()).await
    {
        received += b.len() as u64;
        assert!(b.len() <= MAX_MESSAGE_BYTES);
    }
    assert!(
        received <= WINDOW_BYTES + 64,
        "{received} bytes without an acknowledgement"
    );
    assert!(
        received >= WINDOW_BYTES - 64 * 1024,
        "{received}: the window was filled"
    );
    link.input.send(TerminalInput::Ack(received)).await.unwrap();
    let more = tokio::time::timeout(STEP, link.output.recv())
        .await
        .unwrap();
    assert!(
        matches!(more, Some(TerminalOutput::Bytes(_))),
        "output goes on after an acknowledgement"
    );
    // Ctrl+C ends the flood while output is still being acknowledged.
    link.input
        .send(TerminalInput::Bytes(vec![3]))
        .await
        .unwrap();
    let mut acked = 0;
    loop {
        match tokio::time::timeout(STEP, link.output.recv())
            .await
            .unwrap()
        {
            Some(TerminalOutput::Bytes(b)) => {
                acked += b.len() as u64;
                if String::from_utf8_lossy(&b).contains("^C") {
                    break;
                }
                link.input.send(TerminalInput::Ack(acked)).await.unwrap();
                acked = 0;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn a_stop_ends_the_session_with_its_reason_and_closing_the_link_ends_it_too() {
    let hub = Hub::default();
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    hub.end(&workspace(), EndReason::Stopped);
    assert_eq!(
        the_end(&mut link).await,
        TerminalEnd::of(EndReason::Stopped)
    );

    let mut second = session(&hub, "agent").await;
    bytes_until(&mut second, pty::PROMPT).await;
    hub.end(&WorkspaceId::new("other").unwrap(), EndReason::Stopped);
    drop(second.input);
    let mut gone = second.output;
    // The pump ends once the page's side is gone.
    while let Ok(Some(_)) = tokio::time::timeout(STEP, gone.recv()).await {}
}

#[tokio::test]
async fn a_connection_that_ends_without_a_status_is_the_workspaces_reason() {
    let hub = Hub::default();
    let size = TerminalSize::new(80, 24).unwrap();
    let ident = identity();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let mut link = open(
        &hub,
        Open {
            workspace: &workspace(),
            identity: &ident,
            user: "agent",
            size,
        },
        |stream| async move {
            tokio::select! {
                served = pty::serve(stream) => served,
                _ = rx => Ok(()),
            }
        },
        || EndReason::Crashed,
    )
    .await
    .unwrap();
    bytes_until(&mut link, pty::PROMPT).await;
    tx.send(()).unwrap();
    assert_eq!(
        the_end(&mut link).await,
        TerminalEnd::of(EndReason::Crashed)
    );
}

#[tokio::test]
async fn a_server_that_refuses_before_the_shell_gives_the_boot_reason_cleaned_of_control_characters()
 {
    let hub = Hub::default();
    let ident = identity();
    let err = open(
        &hub,
        Open {
            workspace: &workspace(),
            identity: &ident,
            user: "agent",
            size: TerminalSize::new(80, 24).unwrap(),
        },
        |_stream| async { Err("boot hook failed\n\x1b[31mred".to_owned()) },
        || EndReason::Crashed,
    )
    .await
    .unwrap_err();
    let TerminalError::Ended(end) = err else {
        panic!("{err:?}");
    };
    assert_eq!(end.reason, EndReason::BootFailed);
    assert_eq!(end.message.as_deref(), Some("boot hook failed [31mred"));
}

#[test]
fn the_window_never_lets_an_acknowledgement_exceed_what_was_sent() {
    let mut window = Window::default();
    assert_eq!(window.room(), WINDOW_BYTES);
    window.sent(WINDOW_BYTES);
    assert_eq!(window.room(), 0);
    window.ack(100);
    assert_eq!(window.room(), 100);
    window.ack(u64::MAX);
    assert_eq!(window.room(), WINDOW_BYTES);
}
