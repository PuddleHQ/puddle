// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors the app's pages hit that no code expected: what a page reports, and the recent ones the
//! server kept.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Where the page caught the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UiErrorKind {
    /// A load or a render failed and the page shows its error page.
    Page,
    /// Code outside the page's own handling threw (`window.onerror`).
    Uncaught,
    /// A promise failed and nothing handled it (`unhandledrejection`).
    UnhandledRejection,
}

/// One error a page reports. The server bounds every field, takes the user info, query and
/// fragment out of every URL and removes what looks like a token before it logs or keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UiErrorReport {
    /// Where the page caught it.
    pub kind: UiErrorKind,
    /// The error's name and message, such as `TypeError: x is not a function`.
    pub message: String,
    /// The top of its stack, one frame a line, if it had one.
    #[serde(default)]
    pub stack: Option<String>,
    /// The page's path, such as `/workspaces/abc/terminal` (a query or fragment is dropped).
    #[serde(default)]
    pub route: Option<String>,
    /// The build of the app the page runs.
    #[serde(default)]
    pub build: Option<String>,
}

/// An error a page reported, as the server kept it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UiError {
    /// When the server received it, in epoch milliseconds.
    pub at: u64,
    /// Where the page caught it.
    pub kind: UiErrorKind,
    /// The error's name and message.
    pub message: String,
    /// The top of its stack, or `null`.
    #[schema(required = true)]
    pub stack: Option<String>,
    /// The page's path, or `null`.
    #[schema(required = true)]
    pub route: Option<String>,
    /// The build of the app the page ran, or `null`.
    #[schema(required = true)]
    pub build: Option<String>,
}

/// The errors the caller's pages reported since the server started, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UiErrorList {
    /// The most recent errors, newest first; the oldest make room past the limit.
    pub errors: Vec<UiError>,
    /// How many reports were refused because the pages sent too many at once.
    pub dropped: u64,
}
