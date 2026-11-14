// SPDX-License-Identifier: AGPL-3.0-or-later
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

fn workspace() -> WorkspaceId {
    WorkspaceId::new("demo").unwrap()
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
    let free = puddle_test_kit::ReservedPort::new();
    let addr = SocketAddr::from(([127, 0, 0, 1], free.port()));
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
