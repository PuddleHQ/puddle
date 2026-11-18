// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor under remote serving. A request whose `Host` is a workspace's name, the
//! editor on `<id>.<base>` or a forwarded port on `<port>-<id>.<base>`, goes here instead of to
//! the API's routes, on every listener the API has (the loopback one for a proxy on this machine
//! and the network one for a proxy elsewhere).
//!
//! Every such name gets the same answer, whether or not a workspace has that id: the request has
//! not signed in, so it must not learn which ids exist (another user's included). Nothing reaches
//! a guest without the way in Puddle hands a signed-in browser.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use puddle_front::remote_label::sync_shadow_problems;
use puddle_types::Problems;

use crate::remote::WorkspaceNames;
use crate::workspaces::WorkspaceService;

/// Where requests under the workspaces' names are answered.
pub(crate) struct RemoteEditor {
    names: WorkspaceNames,
    workspaces: Arc<dyn WorkspaceService>,
    problems: Arc<Problems>,
}

impl RemoteEditor {
    /// The editor under `names`, over these workspaces, raising address clashes into `problems`.
    /// The clashes are checked once now, so a host that starts with two clashing workspaces says
    /// so before anyone visits.
    pub(crate) fn new(
        names: WorkspaceNames,
        workspaces: Arc<dyn WorkspaceService>,
        problems: Arc<Problems>,
    ) -> Self {
        let editor = Self {
            names,
            workspaces,
            problems,
        };
        editor.sync_problems();
        editor
    }

    /// Whether the request's one `Host` is a workspace's name. A request with no `Host` or more
    /// than one is the API's to refuse.
    pub(crate) fn claims(&self, headers: &HeaderMap) -> bool {
        one_host(headers).is_some_and(|host| self.names.claims(host))
    }

    /// Brings the address clash problems up to date with the workspaces there are now.
    fn sync_problems(&self) {
        sync_shadow_problems(&self.problems, &self.workspaces.names());
    }

    /// Answers a request [`RemoteEditor::claims`]: the same for every name, so it never says
    /// whether a workspace has that id.
    pub(crate) fn handle(&self) -> Response {
        self.sync_problems();
        text(
            StatusCode::FORBIDDEN,
            "Open a workspace's editor from Puddle.",
        )
    }
}

/// The request's `Host` when it has exactly one, as text.
fn one_host(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::HOST).iter();
    let first = values.next()?;
    values
        .next()
        .is_none()
        .then(|| first.to_str().ok())
        .flatten()
}

/// A plain-text answer that no browser runs as a page and nothing caches.
fn text(status: StatusCode, message: &'static str) -> Response {
    let mut response = Response::new(Body::from(message));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
