// SPDX-License-Identifier: GPL-3.0-or-later
//! The guard in front of every route: `Host`, then `Origin`, then the bearer token.
//! See the crate docs for why each check exists.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::authz::Principal;
use crate::error::{ApiError, ErrorCode};
use crate::remote::RemoteServing;
use crate::token::ApiToken;

/// Which listener a guard stands in front of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Listener {
    /// The loopback listener: the desktop app, a CLI, or a reverse proxy on this machine.
    Loopback,
    /// The network listener for a reverse proxy on another machine. A peer there can never mean
    /// `127.0.0.1`, so a loopback `Host` is refused outright.
    Network,
}

/// What kind of `Host` a request names. The install token is accepted only for [`Self::Loopback`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostClass {
    Loopback,
    Public,
}

/// What remote serving adds to a guard.
#[derive(Clone)]
struct Remote {
    listener: Listener,
    /// The exact public authorities, lower case.
    hosts: Vec<String>,
    /// `https://` plus each authority.
    origins: Vec<String>,
}

/// What a request must match. Built once per server, after the port is known.
#[derive(Clone)]
pub(crate) struct Guard {
    inner: Arc<Inner>,
}

#[derive(Clone)]
struct Inner {
    token: ApiToken,
    /// `127.0.0.1:<port>` and `localhost:<port>`, lower case.
    hosts: [String; 2],
    /// Whether the app's files are served, so reads outside `/api` need no token.
    serves_ui: bool,
    /// The API's own origins plus the configured extra ones, lower case.
    origins: Vec<String>,
    /// Set when remote serving is on.
    remote: Option<Remote>,
    /// Who the holder of the token is.
    principal: Principal,
}

/// Why a request was refused. The reason is logged; header values never are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Host,
    Origin,
    /// An `Origin` request whose `Sec-Fetch-Site` says the browser did not make it from the
    /// page's own origin.
    FetchSite,
    Token,
    /// A token that is valid but not accepted for this kind of `Host`.
    CredentialHost,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Origin => "origin",
            Self::FetchSite => "fetch-site",
            Self::Token => "token",
            Self::CredentialHost => "credential-host",
        }
    }
}

impl From<Refusal> for ApiError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Host => ApiError::new(
                StatusCode::MISDIRECTED_REQUEST,
                ErrorCode::MisdirectedHost,
                "the Host header must be 127.0.0.1:<port> or localhost:<port>",
            ),
            Refusal::Origin | Refusal::FetchSite => ApiError::new(
                StatusCode::FORBIDDEN,
                ErrorCode::ForbiddenOrigin,
                "requests from this origin are not allowed",
            ),
            Refusal::CredentialHost => ApiError::new(
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "this credential is not accepted on this address",
            ),
            Refusal::Token => ApiError::new(
                StatusCode::UNAUTHORIZED,
                ErrorCode::Unauthorized,
                "missing or wrong bearer token",
            ),
        }
    }
}

impl Guard {
    pub(crate) fn new(token: ApiToken, port: u16, extra_origins: &[String]) -> Self {
        let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
        let mut origins: Vec<String> = hosts.iter().map(|h| format!("http://{h}")).collect();
        origins.extend(extra_origins.iter().map(|o| o.to_ascii_lowercase()));
        Self {
            inner: Arc::new(Inner {
                token,
                hosts,
                serves_ui: false,
                origins,
                remote: None,
                principal: Principal::INSTALL,
            }),
        }
    }

    /// Adds remote serving: the exact public authorities and their `https://` origins, for the
    /// listener this guard stands in front of.
    pub(crate) fn remote(mut self, remote: &RemoteServing, listener: Listener) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            let hosts: Vec<String> = remote
                .public_hosts()
                .iter()
                .map(crate::remote::PublicHost::authority)
                .collect();
            let origins = remote
                .public_hosts()
                .iter()
                .map(crate::remote::PublicHost::origin)
                .collect();
            inner.remote = Some(Remote {
                listener,
                hosts,
                origins,
            });
        }
        self
    }

    /// Lets `GET`/`HEAD` outside `/api` through without a token (the app's own files). The
    /// `Host` and `Origin` checks still apply.
    pub(crate) fn serving_ui(mut self, serves_ui: bool) -> Self {
        if let Some(inner) = Arc::get_mut(&mut self.inner) {
            inner.serves_ui = serves_ui;
        }
        self
    }

    /// Makes the token's holder `principal` instead of the owner (tests call the routes as
    /// another user this way).
    pub(crate) fn acting_as(mut self, principal: Principal) -> Self {
        Arc::make_mut(&mut self.inner).principal = principal;
        self
    }

    /// Checks one request's head.
    pub(crate) fn check(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), Refusal> {
        let class = self.check_host(uri, headers)?;
        self.check_origin(headers, class)?;
        self.check_token(headers, class)
    }

    /// Like [`Guard::check`], but a read of the app's own files needs no token. The principal
    /// behind the token, or `None` for a request that carries none.
    fn check_request(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Result<Option<Principal>, Refusal> {
        let is_read = method == Method::GET || method == Method::HEAD;
        if method == Method::GET && is_terminal_socket(uri.path()) {
            // The ticket in `Sec-WebSocket-Protocol` stands in for the token (a browser cannot
            // send it on a WebSocket); the route checks it, after these two.
            let class = self.check_host(uri, headers)?;
            self.check_origin(headers, class)?;
            // A ticket is minted behind the install token and is not bound to a listener, so it
            // must not open a shell through a public name; sign-in for those comes later.
            if class == HostClass::Public {
                return Err(Refusal::CredentialHost);
            }
            Ok(None)
        } else if self.inner.serves_ui && is_read && !is_api_path(uri.path()) {
            let class = self.check_host(uri, headers)?;
            self.check_origin(headers, class)?;
            Ok(None)
        } else {
            self.check(uri, headers)?;
            Ok(Some(self.inner.principal))
        }
    }

    fn check_host(&self, uri: &Uri, headers: &HeaderMap) -> Result<HostClass, Refusal> {
        let host = single(headers, &header::HOST).ok_or(Refusal::Host)?;
        let host = host.to_str().map_err(|_| Refusal::Host)?;
        let class = self.host_class(host).ok_or(Refusal::Host)?;
        // An absolute-form target (`GET http://other/ HTTP/1.1`) must name us too, the same way.
        match uri.authority() {
            Some(authority) if self.host_class(authority.as_str()) != Some(class) => {
                Err(Refusal::Host)
            }
            _ => Ok(class),
        }
    }

    /// What kind of own `Host` this is, if it is one.
    fn host_class(&self, host: &str) -> Option<HostClass> {
        let matches = |own: &String| own.eq_ignore_ascii_case(host);
        let loopback = || {
            self.inner
                .hosts
                .iter()
                .any(matches)
                .then_some(HostClass::Loopback)
        };
        match &self.inner.remote {
            None => loopback(),
            Some(remote) => {
                let public = remote
                    .hosts
                    .iter()
                    .any(matches)
                    .then_some(HostClass::Public);
                public.or_else(|| match remote.listener {
                    Listener::Loopback => loopback(),
                    Listener::Network => None,
                })
            }
        }
    }

    fn check_origin(&self, headers: &HeaderMap, class: HostClass) -> Result<(), Refusal> {
        let mut values = headers.get_all(header::ORIGIN).iter();
        let Some(origin) = values.next() else {
            // Not a browser, or a same-origin GET: the token still decides.
            return Ok(());
        };
        if values.next().is_some() {
            return Err(Refusal::Origin);
        }
        let origin = origin.to_str().map_err(|_| Refusal::Origin)?;
        let own = match (class, &self.inner.remote) {
            (HostClass::Public, Some(remote)) => &remote.origins,
            _ => &self.inner.origins,
        };
        if !own.iter().any(|own| own.eq_ignore_ascii_case(origin)) {
            return Err(Refusal::Origin);
        }
        // Behind a front, a browser says whether the page that made the request is the same
        // origin; anything else is a page of another site driving the user's browser. A client
        // that is not a browser sends no such header.
        if class == HostClass::Public {
            match headers.get("sec-fetch-site") {
                None => {}
                Some(site) if site.as_bytes().eq_ignore_ascii_case(b"same-origin") => {}
                Some(_) => return Err(Refusal::FetchSite),
            }
        }
        Ok(())
    }

    fn check_token(&self, headers: &HeaderMap, class: HostClass) -> Result<(), Refusal> {
        let value = single(headers, &header::AUTHORIZATION).ok_or(Refusal::Token)?;
        let value = value.as_bytes();
        // The scheme is case-insensitive (RFC 9110 §11.1); exactly one space before the token.
        let (scheme, token) = value.split_at_checked(7).ok_or(Refusal::Token)?;
        if !scheme.eq_ignore_ascii_case(b"bearer ") {
            return Err(Refusal::Token);
        }
        if !self.inner.token.matches(token) {
            return Err(Refusal::Token);
        }
        // The install token is the desktop app's and the CLI's. It is not a credential for a
        // browser that came in through a public name.
        match class {
            HostClass::Loopback => Ok(()),
            HostClass::Public => Err(Refusal::CredentialHost),
        }
    }
}

/// The header's value if it appears exactly once.
fn single<'a>(
    headers: &'a HeaderMap,
    name: &header::HeaderName,
) -> Option<&'a header::HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

/// `/api/workspaces/{id}/terminals/{terminal}`, the one path that takes a ticket in place of the
/// token.
fn is_terminal_socket(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    matches!(
        parts.as_slice(),
        ["", "api", "workspaces", id, "terminals", n] if !id.is_empty() && !n.is_empty()
    )
}

/// Whether `path` is the API (`/api` or below), whose every route needs the token.
pub(crate) fn is_api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// axum middleware running [`Guard::check_request`] before the route.
pub(crate) async fn guard(
    State(guard): State<Guard>,
    mut request: Request,
    next: Next,
) -> Response {
    match guard.check_request(request.method(), request.uri(), request.headers()) {
        Ok(principal) => {
            if let Some(principal) = principal {
                request.extensions_mut().insert(principal);
            }
            next.run(request).await
        }
        Err(refusal) => {
            tracing::warn!(
                reason = refusal.reason(),
                method = %request.method(),
                path = request.uri().path(),
                "api request refused"
            );
            ApiError::from(refusal).into_response()
        }
    }
}

#[cfg(test)]
mod tests;
