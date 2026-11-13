// SPDX-License-Identifier: AGPL-3.0-or-later
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

async fn type_line(link: &TerminalLink, text: &str) {
    link.input
        .send(TerminalInput::Bytes(format!("{text}\r").into_bytes()))
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
        // Without a window the flood never goes quiet: stop the loop instead of hanging on it.
        assert!(
            received < 2 * WINDOW_BYTES,
            "{received} bytes and still coming"
        );
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

async fn refused_with(user: &str) -> TerminalEnd {
    let hub = Hub::default();
    let ident = identity();
    let err = open(
        &hub,
        Open {
            workspace: &workspace(),
            identity: &ident,
            user,
            size: TerminalSize::new(80, 24).unwrap(),
        },
        |stream| async move { pty::serve(stream).await },
        || EndReason::Crashed,
    )
    .await
    .unwrap_err();
    let TerminalError::Ended(end) = err else {
        panic!("{err:?}");
    };
    end
}

#[tokio::test]
async fn a_server_that_refuses_the_login_the_pty_or_the_shell_or_hangs_up_says_which() {
    for (user, said) in [
        ("denied", "refused the login as denied"),
        ("nopty", "refused to give a pseudo-terminal"),
        ("noshell", "refused to give a shell"),
        ("hangup", "ended before a pseudo-terminal was given"),
    ] {
        let end = refused_with(user).await;
        assert_eq!(end.reason, EndReason::BootFailed, "{user}");
        let message = end.message.unwrap();
        assert!(message.contains(said), "{user}: {message}");
    }
}

#[test]
fn a_connection_that_ends_for_a_crashed_workspace_says_crashed_and_otherwise_stopped() {
    use puddle_types::WorkspaceStatus;
    assert_eq!(
        reason_for(Some(WorkspaceStatus::Crashed)),
        EndReason::Crashed
    );
    assert_eq!(
        reason_for(Some(WorkspaceStatus::Running)),
        EndReason::Stopped
    );
    assert_eq!(reason_for(None), EndReason::Stopped);
}

#[test]
fn the_hub_forgets_sessions_whose_end_nobody_listens_to_any_more() {
    let hub = Hub::default();
    let _kept = hub.register(&workspace());
    drop(hub.register(&workspace()));
    let _newest = hub.register(&workspace());
    let open = hub.open.lock().unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.iter().all(|(_, sender)| !sender.is_closed()));
}

#[tokio::test]
async fn a_server_that_sends_stray_messages_before_the_shell_still_gives_one() {
    let hub = Hub::default();
    let mut link = session(&hub, "chatty").await;
    type_line(&link, "whoami").await;
    bytes_until(&mut link, "chatty\r\n").await;
}

#[tokio::test(start_paused = true)]
async fn a_workspace_that_is_not_ready_in_time_ends_as_a_boot_failure() {
    let end = wait_until_ready(
        Duration::from_secs(90),
        Duration::from_millis(250),
        || async { Ok::<Option<()>, TerminalError>(None) },
    )
    .await
    .unwrap_err();
    let TerminalError::Ended(end) = end else {
        panic!("{end:?}")
    };
    assert_eq!(end.reason, EndReason::BootFailed);
    assert_eq!(
        end.message.as_deref(),
        Some("the workspace was not ready in 90 seconds")
    );
}

#[tokio::test(start_paused = true)]
async fn the_wait_ends_with_what_the_probe_finds_after_some_looks() {
    let mut looks = 0;
    let found = wait_until_ready(Duration::from_secs(90), Duration::from_millis(250), || {
        looks += 1;
        let ready = looks == 3;
        async move { Ok::<_, TerminalError>(ready.then_some("up")) }
    })
    .await
    .unwrap();
    assert_eq!(found, "up");
    assert_eq!(looks, 3);

    let refused = wait_until_ready::<(), _, _>(Duration::from_secs(90), Duration::ZERO, || async {
        Err(TerminalError::NotRunning("gone".into()))
    })
    .await
    .unwrap_err();
    assert!(
        matches!(refused, TerminalError::NotRunning(_)),
        "{refused:?}"
    );
}

/// Keeps the far end of the connection open for a while after the server function returned.
fn keep_open(stream: tokio::io::DuplexStream) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(5)).await;
        drop(stream);
    });
}

async fn open_with_server<Fut>(serve: impl FnOnce(tokio::io::DuplexStream) -> Fut) -> TerminalEnd
where
    Fut: Future<Output = Result<(), String>> + Send + 'static,
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
        serve,
        || EndReason::Crashed,
    )
    .await
    .unwrap_err();
    let TerminalError::Ended(end) = err else {
        panic!("{err:?}");
    };
    end
}

#[tokio::test]
async fn a_server_that_ends_before_or_after_the_client_gave_up_still_gives_its_reason() {
    // It ends while the client still waits for the server's greeting.
    let early = open_with_server(|stream| async move {
        keep_open(stream);
        Err("the guest is gone".to_owned())
    })
    .await;
    assert_eq!(early.message.as_deref(), Some("the guest is gone"));
    let quiet = open_with_server(|stream| async move {
        keep_open(stream);
        Ok(())
    })
    .await;
    assert!(quiet.message.unwrap().contains("closed the connection"));
    // The client gives up first, and the server's own words follow.
    let later = open_with_server(|stream| async move {
        drop(stream);
        tokio::time::sleep(Duration::from_millis(50)).await;
        Err("the guest said no".to_owned())
    })
    .await;
    assert_eq!(later.message.as_deref(), Some("the guest said no"));
    let silent = open_with_server(|stream| async move {
        drop(stream);
        Ok(())
    })
    .await;
    assert!(silent.message.is_some());
}

#[tokio::test]
async fn a_shell_killed_by_a_signal_ends_with_status_255_and_a_page_that_left_ends_a_flood() {
    let hub = Hub::default();
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "kill").await;
    let end = the_end(&mut link).await;
    assert_eq!((end.reason, end.status), (EndReason::Exited, Some(255)));

    // The page stops reading in the middle of a flood: the session ends and nothing is left.
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "flood").await;
    bytes_until(&mut link, "yyyy").await;
    let TerminalLink { input, output } = link;
    drop(output);
    tokio::time::timeout(STEP, input.closed()).await.unwrap();
}

#[tokio::test]
async fn the_hub_counts_the_sessions_open_and_a_finished_one_leaves_the_count() {
    let hub = Hub::default();
    assert_eq!(hub.count(), 0);
    let mut first = session(&hub, "agent").await;
    let mut second = session(&hub, "agent").await;
    bytes_until(&mut first, pty::PROMPT).await;
    bytes_until(&mut second, pty::PROMPT).await;
    assert_eq!(hub.count(), 2);
    type_line(&first, "exit 0").await;
    the_end(&mut first).await;
    eventually_count(&hub, 1).await;
    hub.end(&workspace(), EndReason::Stopped);
    the_end(&mut second).await;
    eventually_count(&hub, 0).await;
}

/// The count drops when the session task has dropped its end of the hub's channel, a moment after
/// the end reaches the page.
async fn eventually_count(hub: &Hub, wanted: usize) {
    puddle_test_kit::eventually("the hub's open count", STEP, || hub.count() == wanted).await;
}

#[tokio::test]
async fn the_shell_is_asked_for_as_the_session_users_login_shell_in_its_home_directory() {
    let hub = Hub::default();
    let mut link = session(&hub, "agent").await;
    bytes_until(&mut link, pty::PROMPT).await;
    type_line(&link, "command").await;
    bytes_until(&mut link, &format!("{LOGIN_SHELL}\r\n")).await;
}
