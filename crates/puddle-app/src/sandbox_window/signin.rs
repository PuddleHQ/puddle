// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_types::WorkspaceName;
use tauri::webview::cookie::Cookie;
use tokio::net::TcpStream;

/// The cookie the editor server sets when a login succeeds.
pub const SESSION_COOKIE: &str = "code-server-session";

/// How long the editor gets to answer the login.
const TIMEOUT: Duration = Duration::from_secs(15);

/// The editor's session: a secret that signs the window in.
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
    workspace: &WorkspaceName,
    password: &str,
) -> Result<SessionCookie, SignInError> {
    tokio::time::timeout(TIMEOUT, post_login(front, workspace, password))
        .await
        .map_err(|_| SignInError::Timeout)?
}

async fn post_login(
    front: SocketAddr,
    workspace: &WorkspaceName,
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
mod tests {
    use std::sync::{Arc, Mutex};

    use http::Response;
    use hyper::service::service_fn;
    use puddle_types::fixture;
    use tokio::net::TcpListener;

    use super::*;

    /// What the fake editor saw of the login.
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    struct Seen {
        host: String,
        path: String,
        method: String,
        body: String,
    }

    /// A stand-in for the editor behind the front: accepts one password and sets a cookie for it.
    async fn editor(accepts: String) -> (SocketAddr, Arc<Mutex<Seen>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Seen::default()));
        let sink = seen.clone();
        // One login per test, so one connection.
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                let (sink, accepts) = (sink.clone(), accepts.clone());
                async move {
                    let (parts, body) = request.into_parts();
                    let body = body.collect().await.unwrap().to_bytes();
                    let body = String::from_utf8(body.to_vec()).unwrap();
                    *sink.lock().unwrap() = Seen {
                        host: parts.headers[HOST].to_str().unwrap().to_owned(),
                        path: parts.uri.path().to_owned(),
                        method: parts.method.to_string(),
                        body: body.clone(),
                    };
                    let wanted = url::form_urlencoded::Serializer::new(String::new())
                        .append_pair("password", &accepts)
                        .finish();
                    let mut answer = Response::builder();
                    if body == wanted {
                        answer = answer
                            .status(302)
                            .header(SET_COOKIE, "other=1; Path=/")
                            .header(SET_COOKIE, "code-server-session=abc123; Path=/; HttpOnly");
                    } else {
                        answer = answer.status(200);
                    }
                    Ok::<_, std::convert::Infallible>(
                        answer.body(Full::new(Bytes::from_static(b"page"))).unwrap(),
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
        (addr, seen)
    }

    fn workspace() -> WorkspaceName {
        WorkspaceName::new("demo").unwrap()
    }

    #[tokio::test]
    async fn the_login_is_posted_to_the_front_under_the_workspace_s_host_name() {
        let secret = fixture::password("login");
        let (addr, seen) = editor(secret.clone()).await;
        let cookie = sign_in(addr, &workspace(), &secret).await.unwrap();
        assert_eq!(
            (cookie.name(), cookie.value()),
            ("code-server-session", "abc123")
        );
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            Seen {
                host: format!("demo.localhost:{}", addr.port()),
                path: "/login".into(),
                method: "POST".into(),
                body: format!("password={secret}"),
            }
        );
    }

    #[tokio::test]
    async fn a_password_with_special_characters_stays_one_form_value() {
        let special = fixture::password("a&b=c d");
        let (addr, seen) = editor(special.clone()).await;
        sign_in(addr, &workspace(), &special).await.unwrap();
        let encoded = format!("password={}-{}", "a%26b%3Dc+d", fixture::suffix());
        assert_eq!(seen.lock().unwrap().body, encoded);
    }

    #[tokio::test]
    async fn a_login_the_editor_does_not_accept_gives_no_session() {
        let (addr, _) = editor(fixture::password("right")).await;
        let err = sign_in(addr, &workspace(), &fixture::password("wrong"))
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::NoSession(200)), "{err}");
    }

    #[tokio::test]
    async fn a_front_that_is_not_there_is_an_error() {
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        let err = sign_in(addr, &workspace(), &fixture::password("unused"))
            .await
            .unwrap_err();
        assert!(matches!(err, SignInError::Connect(_)), "{err}");
    }

    #[test]
    fn the_cookie_s_debug_form_hides_its_value() {
        let cookie = SessionCookie::new(SESSION_COOKIE, "abc123");
        let shown = format!("{cookie:?}");
        assert!(
            shown.contains("code-server-session") && !shown.contains("abc123"),
            "{shown}"
        );
    }
}
