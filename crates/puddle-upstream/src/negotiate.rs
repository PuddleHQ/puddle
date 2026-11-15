// SPDX-License-Identifier: AGPL-3.0-or-later
//! Negotiate and NTLM sign-in to a proxy, as a [`ProxyAuth`] over a pluggable token source.
//!
//! This module is the protocol half and is portable: which scheme to speak, how a `407`
//! challenge becomes the next `Proxy-Authorization` value, how many legs, what happens when the
//! proxy refuses. The other half, producing the opaque tokens as the logged-on user, is a
//! [`TokenSource`]: Windows SSPI (`windows/sspi.rs`), later GSSAPI on Linux. A
//! source never asks for a password; if the OS has no ticket for the user the session fails.
//!
//! Behaviour:
//!
//! - **Negotiate is preferred over NTLM** when the proxy offers both.
//! - **Preemptive first leg.** Before any `407` (`offered` empty) a session starts on the scheme
//!   last seen for that proxy, else Negotiate, and its first token goes on the first request:
//!   a Kerberos ticket needs no challenge, and an NTLM Type 1 message saves the bare `407`.
//! - **Several legs on one connection** (NTLM: Type 1, then Type 3 after the challenge).
//! - A Negotiate token the proxy refuses, with only NTLM left on offer, restarts the session on
//!   NTLM once. A second refusal is an error, so a wrong sign-in never loops.
//! - Tokens and challenges are never logged or put in errors, only their scheme and length.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::auth::{AuthError, AuthSession, AuthStep, ProxyAuth};
use crate::hop::ProxyAddr;

/// An HTTP authentication scheme this module speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Package {
    /// SPNEGO: Kerberos when the user has a ticket for the proxy, NTLM otherwise.
    Negotiate,
    /// NTLM alone.
    Ntlm,
}

impl Package {
    /// The scheme word in `Proxy-Authenticate` and `Proxy-Authorization`.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            Self::Negotiate => "Negotiate",
            Self::Ntlm => "NTLM",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        if word.eq_ignore_ascii_case("negotiate") {
            Some(Self::Negotiate)
        } else if word.eq_ignore_ascii_case("ntlm") {
            Some(Self::Ntlm)
        } else {
            None
        }
    }
}

/// One output of a security context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leg {
    /// The token to send; empty when the context has nothing more to say.
    pub token: Vec<u8>,
    /// The context is established: no further challenge is expected.
    pub complete: bool,
}

/// A client-side security context for one proxy connection (an SSPI or GSSAPI context).
pub trait SecurityContext: Send + fmt::Debug {
    /// The next leg from the proxy's token (`None` for the first call).
    ///
    /// # Errors
    /// [`AuthError`] when the OS rejects the input or has no credentials.
    fn step(&mut self, input: Option<&[u8]>) -> Result<Leg, AuthError>;
}

/// Opens security contexts as the current user.
pub trait TokenSource: Send + Sync + fmt::Debug {
    /// A fresh context for `package` towards the service `spn` (`HTTP/<proxy host>`).
    ///
    /// # Errors
    /// [`AuthError`] when the package is unavailable or the user has no credentials.
    fn open(&self, package: Package, spn: &str) -> Result<Box<dyn SecurityContext>, AuthError>;
}

/// [`ProxyAuth`] for Negotiate and NTLM over a [`TokenSource`].
#[derive(Debug)]
pub struct NegotiateAuth {
    source: Arc<dyn TokenSource>,
    /// The scheme each proxy last challenged with, so the next connection starts on it.
    learned: Arc<Mutex<HashMap<ProxyAddr, Learned>>>,
}

/// What a proxy's last `407` taught us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Learned {
    /// It asks for this scheme.
    Scheme(Package),
    /// It names no scheme we speak (Basic only): no preemptive token on it again.
    Neither,
}

impl NegotiateAuth {
    /// Authentication through `source`.
    #[must_use]
    pub fn new(source: Arc<dyn TokenSource>) -> Self {
        Self {
            source,
            learned: Arc::default(),
        }
    }
}

impl ProxyAuth for NegotiateAuth {
    fn methods(&self) -> Vec<&'static str> {
        vec!["negotiate", "ntlm"]
    }

    fn network_changed(&self) {
        self.learned
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    fn begin(
        &self,
        proxy: &ProxyAddr,
        offered: &[&str],
    ) -> Result<Option<Box<dyn AuthSession>>, AuthError> {
        let package = if offered.is_empty() {
            let learned = self.learned.lock().unwrap_or_else(PoisonError::into_inner);
            match learned.get(proxy) {
                Some(Learned::Neither) => return Ok(None),
                Some(Learned::Scheme(package)) => *package,
                None => Package::Negotiate,
            }
        } else {
            let known: Vec<Package> = offered
                .iter()
                .filter_map(|w| Package::from_word(w))
                .collect();
            match prefer(&known) {
                Some(package) => package,
                None => return Ok(None),
            }
        };
        let spn = format!("HTTP/{}", proxy.host());
        let context = self.source.open(package, &spn)?;
        Ok(Some(Box::new(Session {
            source: Arc::clone(&self.source),
            learned: Arc::clone(&self.learned),
            proxy: proxy.clone(),
            spn,
            package,
            context,
            legs: 0,
            complete: false,
            tried: vec![package],
        })))
    }
}

fn prefer(known: &[Package]) -> Option<Package> {
    if known.contains(&Package::Negotiate) {
        Some(Package::Negotiate)
    } else if known.contains(&Package::Ntlm) {
        Some(Package::Ntlm)
    } else {
        None
    }
}

struct Session {
    source: Arc<dyn TokenSource>,
    learned: Arc<Mutex<HashMap<ProxyAddr, Learned>>>,
    proxy: ProxyAddr,
    spn: String,
    package: Package,
    context: Box<dyn SecurityContext>,
    /// Tokens sent with the current package.
    legs: u32,
    /// The context reported itself established.
    complete: bool,
    /// Packages used so far, so a refusal restarts at most once per package.
    tried: Vec<Package>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("proxy", &self.proxy)
            .field("package", &self.package)
            .field("legs", &self.legs)
            .finish_non_exhaustive()
    }
}

/// A scheme in a `Proxy-Authenticate` value, with its decoded token when it carries one.
#[derive(Debug, PartialEq, Eq)]
struct Offer {
    package: Package,
    token: Option<Vec<u8>>,
}

/// The schemes this module speaks in a `Proxy-Authenticate` value (one header, or several joined
/// by commas). Unknown schemes are skipped. A token that is not base64 is an error.
fn parse_challenge(value: &str) -> Result<Vec<Offer>, AuthError> {
    let mut offers = Vec::new();
    // Base64 never contains a comma, so a comma always starts the next challenge.
    for part in value.split(',') {
        let mut words = part.split_whitespace();
        let Some(package) = words.next().and_then(Package::from_word) else {
            continue;
        };
        let token = match words.next() {
            None => None,
            Some(text) => Some(STANDARD.decode(text).map_err(|_| {
                AuthError::Failed(format!("malformed {} challenge", package.word()))
            })?),
        };
        offers.push(Offer { package, token });
    }
    Ok(offers)
}

impl Session {
    fn restart(&mut self, package: Package) -> Result<(), AuthError> {
        self.context = self.source.open(package, &self.spn)?;
        self.package = package;
        self.tried.push(package);
        self.legs = 0;
        self.complete = false;
        Ok(())
    }

    fn learn(&self, what: Learned) {
        let mut learned = self.learned.lock().unwrap_or_else(PoisonError::into_inner);
        learned.insert(self.proxy.clone(), what);
    }

    fn refused(&self) -> AuthError {
        AuthError::Failed(format!(
            "the proxy refused the {} sign-in",
            self.package.word()
        ))
    }

    /// Picks what to feed the context from the proxy's offers: the token to answer, after
    /// switching package when the proxy asks for another one.
    fn input_from(&mut self, offers: &[Offer]) -> Result<Option<Vec<u8>>, AuthError> {
        if self.legs == 0 && !self.complete {
            // First token for this package. A 407 that names other schemes decides which.
            let wanted = prefer(&offers.iter().map(|o| o.package).collect::<Vec<_>>());
            match wanted {
                None => {
                    return Err(AuthError::Failed(
                        "no supported sign-in scheme offered".into(),
                    ));
                }
                Some(package) if package != self.package => self.restart(package)?,
                Some(_) => {}
            }
        }
        let mine = offers.iter().find(|o| o.package == self.package);
        match mine.and_then(|o| o.token.clone()) {
            Some(token) => Ok(Some(token)),
            None if self.legs == 0 => Ok(None),
            None => {
                // We already sent a token and the proxy asks again without one: refused.
                let fallback = offers
                    .iter()
                    .map(|o| o.package)
                    .find(|p| !self.tried.contains(p));
                match fallback {
                    Some(package) => {
                        self.restart(package)?;
                        Ok(None)
                    }
                    None => Err(self.refused()),
                }
            }
        }
    }
}

impl AuthSession for Session {
    fn step(&mut self, challenge: Option<&str>) -> Result<AuthStep, AuthError> {
        let input = match challenge {
            None => None,
            Some(value) => {
                let offers = parse_challenge(value)?;
                if offers.is_empty() {
                    self.learn(Learned::Neither);
                    return Err(AuthError::Failed(
                        "no supported sign-in scheme offered".into(),
                    ));
                }
                if self.complete {
                    return Err(self.refused());
                }
                if let Some(first) = prefer(&offers.iter().map(|o| o.package).collect::<Vec<_>>()) {
                    self.learn(Learned::Scheme(first));
                }
                self.input_from(&offers)?
            }
        };
        let leg = self.context.step(input.as_deref())?;
        self.complete = leg.complete;
        if leg.token.is_empty() {
            return if leg.complete {
                Ok(AuthStep::Done)
            } else {
                Err(AuthError::Failed(format!(
                    "{} produced no token",
                    self.package.word()
                )))
            };
        }
        self.legs += 1;
        tracing::debug!(
            proxy = %self.proxy,
            scheme = self.package.word(),
            leg = self.legs,
            token_len = leg.token.len(),
            "proxy sign-in leg"
        );
        Ok(AuthStep::Authorization(format!(
            "{} {}",
            self.package.word(),
            STANDARD.encode(&leg.token)
        )))
    }
}

#[cfg(test)]
mod tests;
