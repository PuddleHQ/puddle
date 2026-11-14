// SPDX-License-Identifier: AGPL-3.0-or-later
//! Terminals: a shell in a running workspace, reached over a WebSocket.
//!
//! `POST` asks for one and answers with a single-use ticket; the socket then opens with the
//! ticket in `Sec-WebSocket-Protocol`, never in the URL. Bytes travel as binary messages, control
//! as JSON text messages.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use utoipa::ToSchema;

use crate::ApiErrorBody;
use crate::auth::ReachedPublicly;
use crate::authz::{Principal, Via};
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json as BodyJson, Path};
use crate::routes::AppState;
use crate::routes::identities::workspace;
use crate::terminals::{
    EndReason, MAX_MESSAGE_BYTES, SUBPROTOCOL, TerminalEnd, TerminalError, TerminalInput,
    TerminalLink, TerminalOutput, TerminalSize,
};

/// The size a new terminal starts with.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalRequest {
    /// Columns, 2 to 500.
    pub(crate) cols: u32,
    /// Rows, 1 to 300.
    pub(crate) rows: u32,
}

/// A terminal that is asked for and not open yet.
#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct TerminalTicket {
    /// The terminal's id in the socket's path.
    pub(crate) terminal: u64,
    /// 64 hex characters: use it once, within 30 s, as `ticket.<hex>` after `puddle-terminal.v1`
    /// in `Sec-WebSocket-Protocol`.
    pub(crate) ticket: String,
}

/// What the page sends as text.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Resize { cols: u32, rows: u32 },
    Ack { bytes: u64 },
}

/// What the host sends as text.
#[derive(Debug, Serialize)]
struct Ended<'a> {
    r#type: &'static str,
    reason: EndReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

impl From<TerminalError> for ApiError {
    fn from(err: TerminalError) -> Self {
        match err {
            TerminalError::Unavailable(m) => {
                Self::new(StatusCode::SERVICE_UNAVAILABLE, ErrorCode::Unavailable, m)
            }
            TerminalError::NotRunning(m) => Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, m),
            TerminalError::Ended(end) => Self::new(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                end.message.unwrap_or_else(|| "the terminal ended".into()),
            ),
        }
    }
}

/// Asks for a terminal in a running or starting workspace. The session opens when the socket
/// connects, so a ticket that is never used leaves nothing behind.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/terminals",
    tag = "terminals",
    params(("id" = String, Path, description = "workspace id")),
    request_body = TerminalRequest,
    responses(
        (status = CREATED, description = "the terminal and its ticket", body = TerminalTicket),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or starting, or the limit of 8 terminals for it or 32 in all is reached (the message says which)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a size outside 2 to 500 columns and 1 to 300 rows", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "terminals are not available in this build", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_terminal(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    BodyJson(body): BodyJson<TerminalRequest>,
) -> Result<(StatusCode, Json<TerminalTicket>), ApiError> {
    let workspace = workspace(&state, &id).await?;
    let size = TerminalSize::new(body.cols, body.rows)
        .ok_or_else(|| ApiError::invalid("a terminal is 2 to 500 columns and 1 to 300 rows"))?;
    state.terminal_service.check(&workspace).await?;
    let ticket = new_ticket()?;
    let terminal = state
        .terminals
        .create(&workspace, size, ticket.clone())
        .map_err(|limit| {
            ApiError::new(StatusCode::CONFLICT, ErrorCode::Conflict, limit.to_string())
        })?;
    if let Via::Device(device) = principal.via {
        state.terminals.bind(terminal, device);
    }
    Ok((
        StatusCode::CREATED,
        Json(TerminalTicket { terminal, ticket }),
    ))
}

fn new_ticket() -> Result<String, ApiError> {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| ApiError::internal(&e))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    }))
}

/// The ticket among the offered sub-protocols (`puddle-terminal.v1, ticket.<hex>`), when the
/// request names the sub-protocol too.
fn ticket_of(headers: &HeaderMap) -> Option<&str> {
    let offered: Vec<&str> = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    let ticket = offered.iter().find_map(|p| p.strip_prefix("ticket."))?;
    offered.contains(&SUBPROTOCOL).then_some(ticket)
}

/// Opens the terminal's socket. A browser cannot send `Authorization` on a WebSocket, so on this
/// one path the single-use ticket stands in for the token; the `Host` and `Origin` checks run
/// before it is read, and the ticket is checked before the workspace is looked up, so a caller
/// without one learns nothing about which workspaces exist.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/terminals/{terminal}",
    tag = "terminals",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("terminal" = u64, Path, description = "the terminal id from the ticket")
    ),
    responses(
        (status = SWITCHING_PROTOCOLS, description = "the socket: binary messages carry the terminal's bytes (at most 64 KiB), text messages are JSON (`resize`, `ack` from the page; `ended` from the host)"),
        (status = UNAUTHORIZED, description = "a wrong, used or late ticket, or a loopback ticket used under a public name", body = ApiErrorBody)
    )
)]
pub(crate) async fn terminal_socket(
    State(state): State<AppState>,
    Path((id, terminal)): Path<(String, u64)>,
    headers: HeaderMap,
    public: Option<axum::Extension<ReachedPublicly>>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return Ok(rejection.into_response()),
    };
    // The registry holds the workspace a ticket was issued for, so a claim also proves it exists;
    // a malformed id and an unknown one both fail the claim, with the same answer.
    // Under a public name only a ticket a signed-in browser asked for counts: one made with the
    // install token on loopback is not bound to a device and never opens a shell there.
    let admitted = public.is_none() || state.terminals.device_of(terminal).is_some();
    let claimed = WorkspaceId::new(&id)
        .ok()
        .filter(|_| admitted)
        .and_then(|workspace| {
            let size = ticket_of(&headers).and_then(|ticket| {
                state
                    .terminals
                    .claim(&workspace, terminal, ticket.as_bytes())
            })?;
            Some((workspace, size))
        });
    let (workspace, size) = claimed.ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "missing, wrong, used or late ticket",
        )
    })?;
    let shutdown = state.shutdown.clone();
    // A terminal made for a signed-in browser ends when that browser is signed out.
    let ended: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> =
        match (state.terminals.device_of(terminal), &state.sign_in) {
            (Some(device), Some(sign_in)) => Box::pin(sign_in.clone().ended(device)),
            _ => Box::pin(std::future::pending()),
        };
    Ok(upgrade
        .protocols([SUBPROTOCOL])
        .max_message_size(MAX_MESSAGE_BYTES + 1024)
        .on_upgrade(move |socket| async move {
            run(socket, &state, &workspace, size, shutdown, ended).await;
            state.terminals.release(terminal);
        }))
}

async fn run(
    mut socket: WebSocket,
    state: &AppState,
    workspace: &WorkspaceId,
    size: TerminalSize,
    shutdown: watch::Receiver<bool>,
    ended: impl Future<Output = ()> + Unpin,
) {
    let link = match state.terminal_service.open(workspace, size).await {
        Ok(link) => link,
        Err(err) => {
            let end = match err {
                TerminalError::Ended(end) => end,
                other => TerminalEnd {
                    reason: EndReason::Stopped,
                    status: None,
                    message: Some(other.to_string()),
                },
            };
            finish(&mut socket, &end).await;
            return;
        }
    };
    pump(&mut socket, link, shutdown, ended).await;
}

async fn finish(socket: &mut WebSocket, end: &TerminalEnd) {
    let text = serde_json::to_string(&Ended {
        r#type: "ended",
        reason: end.reason,
        status: end.status,
        message: end.message.as_deref(),
    })
    .unwrap_or_default();
    // The page may have gone already; there is no one to tell.
    let _ = socket.send(Message::Text(text.into())).await;
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::NORMAL,
            reason: "".into(),
        })))
        .await;
    await_close(socket).await;
}

/// How long a closed socket waits for the page's own close.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// Reads what the page still sends until the closing handshake ends. Dropping a socket with unread
/// data from the page resets the connection, and the reset throws away what the page has not read
/// yet: the end message, which would then show as a broken connection instead of how the shell ended.
async fn await_close(socket: &mut WebSocket) {
    let _ = tokio::time::timeout(CLOSE_WAIT, async {
        while let Some(Ok(_)) = socket.recv().await {}
    })
    .await;
}

async fn refuse(socket: &mut WebSocket) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::POLICY,
            reason: "bad message".into(),
        })))
        .await;
    await_close(socket).await;
}

/// What one message from the page asks for.
enum FromPage {
    /// Something for the session.
    Input(TerminalInput),
    /// A ping or a pong: nothing.
    Keepalive,
    /// Not allowed: the socket closes.
    Refused,
}

fn from_page(message: Message) -> FromPage {
    let input = match message {
        Message::Binary(bytes) if bytes.len() <= MAX_MESSAGE_BYTES => {
            TerminalInput::Bytes(bytes.to_vec())
        }
        Message::Text(text) if text.len() <= MAX_MESSAGE_BYTES => {
            match serde_json::from_str::<Control>(&text) {
                Ok(Control::Resize { cols, rows }) => match TerminalSize::new(cols, rows) {
                    Some(size) => TerminalInput::Resize(size),
                    None => return FromPage::Refused,
                },
                Ok(Control::Ack { bytes }) => TerminalInput::Ack(bytes),
                Err(_) => return FromPage::Refused,
            }
        }
        Message::Ping(_) | Message::Pong(_) => return FromPage::Keepalive,
        _ => return FromPage::Refused,
    };
    FromPage::Input(input)
}

async fn pump(
    socket: &mut WebSocket,
    link: TerminalLink,
    mut shutdown: watch::Receiver<bool>,
    mut ended: impl Future<Output = ()> + Unpin,
) {
    let TerminalLink { input, mut output } = link;
    loop {
        tokio::select! {
            () = &mut ended => {
                // The browser was signed out: close at once, with no word of what the shell said.
                let _ = socket
                    .send(Message::Close(Some(CloseFrame {
                        code: close_code::POLICY,
                        reason: "signed out".into(),
                    })))
                    .await;
                await_close(socket).await;
                return;
            }
            () = async { let _ = shutdown.wait_for(|stop| *stop).await; } => {
                finish(socket, &TerminalEnd::of(EndReason::Quitting)).await;
                return;
            }
            message = socket.recv() => {
                let message = match message {
                    Some(Ok(message)) => message,
                    // A message past the socket's own limit: say so before the connection goes. When the
                    // stream simply ended, the send fails and nothing is lost.
                    Some(Err(_)) | None => {
                        let _ = socket
                            .send(Message::Close(Some(CloseFrame {
                                code: close_code::SIZE,
                                reason: "message too big".into(),
                            })))
                            .await;
                        return;
                    }
                };
                if matches!(message, Message::Close(_)) {
                    // The page started the closing handshake. Reading on sends the reply the socket
                    // queued; without it the page sees the connection reset, not a clean end.
                    await_close(socket).await;
                    return;
                }
                match from_page(message) {
                    // A closed session's end is waiting in `output`.
                    FromPage::Input(item) => drop(input.send(item).await),
                    FromPage::Keepalive => {}
                    FromPage::Refused => {
                        refuse(socket).await;
                        return;
                    }
                }
            }
            item = output.recv() => match item {
                Some(TerminalOutput::Bytes(bytes)) => {
                    for chunk in bytes.chunks(MAX_MESSAGE_BYTES) {
                        if socket.send(Message::Binary(chunk.to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                }
                Some(TerminalOutput::Ended(end)) => {
                    finish(socket, &end).await;
                    return;
                }
                None => {
                    let end = TerminalEnd {
                        reason: EndReason::Crashed,
                        status: None,
                        message: Some("the terminal's session ended without saying why".into()),
                    };
                    finish(socket, &end).await;
                    return;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests;
