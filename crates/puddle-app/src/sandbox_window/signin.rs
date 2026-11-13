// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signing in to a workspace's editor on the window's behalf.
//!
//! The bundled editor server asks for a password and answers a login with a session cookie. The
//! window should not show a login page, and the password must not travel in a URL, so the shell
//! posts the login to the front itself and hands the cookie to the window before it navigates.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HOST, SET_COOKIE};
use http::{Method, Request};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use puddle_types::WorkspaceId;
use tauri::webview::cookie::Cookie;
use tokio::net::TcpStream;

/// The cookie the editor server sets when a login succeeds.
pub const SESSION_COOKIE: &str = "code-server-session";

/// How long the editor gets to answer the login.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The editor's session: a secret that signs the window in. The value is overwritten when this
/// drops.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionCookie {
    name: String,
    value: String,
    script_reads: bool,
}

impl SessionCookie {
    /// A session with this name and value, which the page's script cannot read (`HttpOnly`).
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            script_reads: false,
        }
    }

    /// Microsoft's server's connection token, which the workbench's script reads from the cookie
    /// for its socket's handshake, so it is not `HttpOnly`.
    #[must_use]
    pub fn connection_token(token: impl Into<String>) -> Self {
        Self {
            name: puddle_api::CONNECTION_TOKEN_COOKIE.to_owned(),
            value: token.into(),
            script_reads: true,
        }
    }

    /// The cookie's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The cookie's value; never log it.
    #[must_use]
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Whether the page's script reads it (not `HttpOnly`).
    #[must_use]
    pub fn script_reads(&self) -> bool {
        self.script_reads
    }
}

impl Drop for SessionCookie {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.value);
    }
}

impl std::fmt::Debug for SessionCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionCookie")
            .field("name", &self.name)
            .field("value", &"<hidden>")
            .field("script_reads", &self.script_reads)
            .finish()
    }
}

/// Why a sign-in did not give a session.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SignInError {
    /// The front could not be reached or ended the connection.
    #[error("the front could not be reached: {0}")]
    Connect(#[from] std::io::Error),
    /// The exchange with the front failed.
    #[error("the editor's login failed: {0}")]
    Http(#[from] hyper::Error),
    /// The request could not be made.
    #[error("the login request was malformed: {0}")]
    Request(#[from] http::Error),
    /// The editor did not answer in time.
    #[error("the editor did not answer the login in time")]
    Timeout,
    /// The answer had no session cookie (the password was not accepted, or this is not an editor
    /// that asks for one).
    #[error("the editor gave no session (answer {0})")]
    NoSession(u16),
}

/// Posts the login of `workspace`'s editor to the front at `front` and returns the session cookie.
///
/// # Errors
///
/// [`SignInError`] when the front or the editor fails, or the editor sets no session.
pub async fn sign_in(
    front: SocketAddr,
    workspace: &WorkspaceId,
    password: &str,
) -> Result<SessionCookie, SignInError> {
    tokio::time::timeout(TIMEOUT, post_login(front, workspace, password))
        .await
        .map_err(|_| SignInError::Timeout)?
}

async fn post_login(
    front: SocketAddr,
    workspace: &WorkspaceId,
    password: &str,
) -> Result<SessionCookie, SignInError> {
    let stream = TcpStream::connect(front).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    let driver = tokio::spawn(connection);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("password", password)
        .finish();
    let request = Request::builder()
        .method(Method::POST)
        .uri("/login")
        .header(HOST, format!("{workspace}.localhost:{}", front.port()))
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Full::new(Bytes::from(body)))?;
    let response = sender.send_request(request).await?;
    let status = response.status().as_u16();
    let cookie = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| Cookie::parse(v.to_owned()).ok())
        .find(|c| c.name() == SESSION_COOKIE && !c.value().is_empty())
        .map(|c| SessionCookie::new(c.name(), c.value()));
    // The answer's body is a redirect page or an error page: nothing to read.
    let _ = response.into_body().collect().await;
    driver.abort();
    cookie.ok_or(SignInError::NoSession(status))
}

#[cfg(test)]
mod tests;
