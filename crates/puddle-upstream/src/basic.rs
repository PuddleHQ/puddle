// SPDX-License-Identifier: AGPL-3.0-or-later
//! Basic authentication to an upstream proxy from credentials the user configured, and a list
//! that tries several [`ProxyAuth`] implementations in order (Basic after SSPI, say).
//!
//! The credentials are the only ones Puddle ever holds for a proxy; they come from settings or
//! the OS credential store, never from a prompt in the proxy path. They are never logged: every
//! `Debug` shows names, not secrets.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use zeroize::{Zeroize, Zeroizing};

use crate::auth::{AuthError, AuthSession, AuthStep, ProxyAuth};
use crate::hop::ProxyAddr;

/// A user name and password for one proxy (or the default for every proxy). Both are overwritten
/// when the value drops.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    user: String,
    password: String,
}

impl Credentials {
    /// Credentials for `user`.
    #[must_use]
    pub fn new(user: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            user: user.into(),
            password: password.into(),
        }
    }

    /// The user name. The password is never handed out.
    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    /// The `Proxy-Authorization` value, `Basic base64(user:password)`. The `user:password`
    /// temporary and the value itself are sized first, so nothing grows and frees an unwiped
    /// smaller copy, and both are overwritten when they drop.
    fn header(&self) -> Zeroizing<String> {
        const PREFIX: &str = "Basic ";
        let mut plain = Zeroizing::new(String::with_capacity(
            self.user.len() + 1 + self.password.len(),
        ));
        plain.push_str(&self.user);
        plain.push(':');
        plain.push_str(&self.password);
        let encoded_len = base64::encoded_len(plain.len(), true).unwrap_or(plain.len() * 2);
        let mut header = Zeroizing::new(String::with_capacity(PREFIX.len() + encoded_len));
        header.push_str(PREFIX);
        BASE64.encode_string(plain.as_bytes(), &mut header);
        header
    }
}

impl Drop for Credentials {
    fn drop(&mut self) {
        self.user.zeroize();
        self.password.zeroize();
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Where a proxy's Basic credentials are looked up when it asks for them: the product keeps them
/// in the operating system's credential store and answers from memory, so this never blocks.
/// Called once per `407`.
pub trait CredentialSource: Send + Sync + fmt::Debug {
    /// The credentials for `proxy`, if there are any.
    fn credentials(&self, proxy: &ProxyAddr) -> Option<Credentials>;
}

/// Basic authentication: answers a proxy's `407` that offers `Basic` with the configured
/// credentials for that proxy (set by hand, or from a [`CredentialSource`]), else the default
/// credentials.
///
/// A proxy that rejects the credentials is not asked again on the same exchange: the second
/// `407` ends the session with an error, so a wrong password costs one attempt, not a loop
/// (and not an account lockout).
#[derive(Debug, Default, Clone)]
pub struct BasicAuth {
    default: Option<Credentials>,
    per_proxy: HashMap<ProxyAddr, Credentials>,
    source: Option<Arc<dyn CredentialSource>>,
}

impl BasicAuth {
    /// No credentials: [`ProxyAuth::begin`] never offers a session.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Credentials for every proxy without its own.
    #[must_use]
    pub fn with_default(mut self, credentials: Credentials) -> Self {
        self.default = Some(credentials);
        self
    }

    /// Credentials for `proxy` only.
    #[must_use]
    pub fn with_proxy(mut self, proxy: ProxyAddr, credentials: Credentials) -> Self {
        self.per_proxy.insert(proxy, credentials);
        self
    }

    /// Credentials looked up per proxy when it asks: after the ones set with
    /// [`BasicAuth::with_proxy`], before the default.
    #[must_use]
    pub fn with_source(mut self, source: Arc<dyn CredentialSource>) -> Self {
        self.source = Some(source);
        self
    }
}

impl ProxyAuth for BasicAuth {
    fn methods(&self) -> Vec<&'static str> {
        if self.default.is_some() || !self.per_proxy.is_empty() || self.source.is_some() {
            vec!["basic"]
        } else {
            Vec::new()
        }
    }

    fn begin(
        &self,
        proxy: &ProxyAddr,
        offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        // Never preemptive: with nothing offered the proxy may not want credentials at all, and
        // a password must not go to a proxy that did not ask for Basic.
        if !offered.iter().any(|s| s.eq_ignore_ascii_case("basic")) {
            return Ok(None);
        }
        let looked_up = if self.per_proxy.contains_key(proxy) {
            None
        } else {
            self.source.as_ref().and_then(|s| s.credentials(proxy))
        };
        let credentials = self
            .per_proxy
            .get(proxy)
            .or(looked_up.as_ref())
            .or(self.default.as_ref());
        Ok(credentials.map(|credentials| {
            Box::new(BasicSession {
                header: credentials.header(),
                sent: false,
            }) as Box<dyn AuthSession>
        }))
    }
}

struct BasicSession {
    header: Zeroizing<String>,
    sent: bool,
}

impl fmt::Debug for BasicSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BasicSession")
            .field("sent", &self.sent)
            .finish_non_exhaustive()
    }
}

impl AuthSession for BasicSession {
    fn step(&mut self, _challenge: Option<&str>) -> Result<AuthStep, AuthError> {
        if self.sent {
            return Err(AuthError::Failed(
                "the proxy rejected the configured credentials".into(),
            ));
        }
        self.sent = true;
        Ok(AuthStep::Authorization((*self.header).clone()))
    }
}

/// Several [`ProxyAuth`] implementations tried in order: the first that offers a session for
/// the proxy and its offered schemes is used (SSPI first, then Basic from settings).
#[derive(Debug, Default, Clone)]
pub struct AuthList {
    members: Vec<Arc<dyn ProxyAuth>>,
}

impl AuthList {
    /// An empty list: no authentication.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `auth` after the ones already in the list.
    #[must_use]
    pub fn with(mut self, auth: Arc<dyn ProxyAuth>) -> Self {
        self.members.push(auth);
        self
    }
}

impl ProxyAuth for AuthList {
    fn methods(&self) -> Vec<&'static str> {
        let mut all: Vec<&'static str> = Vec::new();
        for method in self.members.iter().flat_map(|member| member.methods()) {
            if !all.contains(&method) {
                all.push(method);
            }
        }
        all
    }

    fn begin(
        &self,
        proxy: &ProxyAddr,
        offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        for member in &self.members {
            if let Some(session) = member.begin(proxy, offered)? {
                return Ok(Some(session));
            }
        }
        Ok(None)
    }

    fn network_changed(&self) {
        for member in &self.members {
            member.network_changed();
        }
    }
}

#[cfg(test)]
mod tests;
