// SPDX-License-Identifier: AGPL-3.0-or-later
//! The seam for authenticating to an upstream proxy (Windows SSPI Negotiate today,
//! GSSAPI Kerberos on Linux later). Discovery names the proxy ([`ProxyAddr`]); this
//! trait turns a proxy's `407` challenge into the next `Proxy-Authorization` value, so the code
//! that connects (CONNECT, absolute-form HTTP) does not know which scheme or OS is behind it.
//!
//! Credentials never pass through this interface: an implementation signs in as the current user
//! (never a prompt) and holds whatever it needs itself.

use std::sync::Arc;

use crate::hop::ProxyAddr;

/// Why authentication could not continue.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// The proxy's challenge could not be answered (bad token, no ticket).
    #[error("proxy authentication failed: {0}")]
    Failed(String),
}

/// What to do after a `407` round trip.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuthStep {
    /// Send the request again with this `Proxy-Authorization` header value.
    Authorization(String),
    /// Authentication is complete; nothing more to send.
    Done,
}

/// One authentication exchange with one proxy, possibly several legs on one connection (NTLM).
pub trait AuthSession: Send + std::fmt::Debug {
    /// The next step. `challenge` is the `Proxy-Authenticate` value of the latest `407` (when the
    /// proxy sent several headers they are joined with `, `, which HTTP allows), or `None` for
    /// the first leg (a preemptive Kerberos token needs no challenge).
    ///
    /// # Errors
    /// [`AuthError`] when the challenge cannot be answered.
    fn step(&mut self, challenge: Option<&str>) -> Result<AuthStep, AuthError>;
}

/// Starts authentication with a proxy.
pub trait ProxyAuth: Send + Sync + std::fmt::Debug {
    /// A session for `proxy` when this implementation speaks one of the `offered` schemes (the
    /// scheme words of the `Proxy-Authenticate` headers, such as `Negotiate`, `NTLM`, `Basic`;
    /// empty before the first `407`). `None` when it cannot help: the caller then reports the
    /// proxy's refusal as it is.
    ///
    /// # Errors
    /// [`AuthError`] when a session cannot be created.
    fn begin(
        &self,
        proxy: &ProxyAddr,
        offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError>;

    /// The scheme words this implementation can answer with (lower case: `negotiate`, `ntlm`,
    /// `basic`), for the network-health report. Empty when it signs in with nothing.
    fn methods(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// The network changed: forget what was learned about each proxy (which scheme it asks for),
    /// since the proxy a name or address stands for may be another one now. Called by the
    /// [`Chain`](crate::Chain) before the first connection of a new epoch.
    fn network_changed(&self) {}
}

/// No authentication: every proxy is used as it is. The Unix implementation of [`system_auth`].
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAuth;

impl ProxyAuth for NoAuth {
    fn begin(
        &self,
        _proxy: &ProxyAddr,
        _offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        Ok(None)
    }
}

/// The authentication of the current platform: Windows SSPI Negotiate and NTLM as the logged-on
/// user. [`NoAuth`] elsewhere; a GSSAPI [`TokenSource`](crate::TokenSource) for
/// [`NegotiateAuth`](crate::NegotiateAuth) is the Linux follow-up.
#[must_use]
pub fn system_auth() -> Arc<dyn ProxyAuth> {
    #[cfg(windows)]
    {
        Arc::new(crate::negotiate::NegotiateAuth::new(Arc::new(
            crate::windows::SspiSource,
        )))
    }
    #[cfg(not(windows))]
    {
        Arc::new(NoAuth)
    }
}

#[cfg(test)]
mod tests;
