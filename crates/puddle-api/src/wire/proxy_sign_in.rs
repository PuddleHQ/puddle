// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signing in to a company proxy that asks for a user name and password (Basic). The password is
//! written once and never comes back: no response carries it.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::environment::SecretText;

/// A stored proxy sign-in. Names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StoredSignIn {
    /// `host:port` of the proxy, lower case.
    pub proxy: String,
    /// The user name the proxy is signed in to with.
    pub user: String,
}

/// The proxies Puddle holds a sign-in for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProxySignInList {
    /// Sorted by proxy.
    pub sign_ins: Vec<StoredSignIn>,
}

/// Stores or replaces the sign-in for one proxy.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProxySignInRequest {
    /// The proxy as `host:port`, or `http://host:port`; a missing port is 80.
    pub proxy: String,
    /// The user name: not empty, at most 256 bytes, no colon or line break (Basic cannot carry
    /// them).
    pub user: String,
    /// The password: not empty, no line break. Written once to the operating system's credential
    /// store and never shown again.
    #[schema(value_type = String, format = Password, write_only)]
    pub password: SecretText,
}
