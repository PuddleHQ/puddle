// SPDX-License-Identifier: AGPL-3.0-or-later
//! Remote serving: the settings that let a reverse proxy in front of Puddle reach the API, and
//! the names a request may use then.
//!
//! Off, none of this exists: the API answers `127.0.0.1:<port>` and `localhost:<port>` only.
//! On, the API also answers the exact public authorities the operator lists, and may bind a
//! second listener on one chosen network interface for a proxy on another machine
//! ([`NetworkListener`]).

use std::fmt::{self, Write as _};
use std::net::IpAddr;

use puddle_types::DomainName;
use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

/// What makes a remote-serving setting unusable. The text is the whole message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RemoteError {
    /// A public host that is not a bare host name or IP address with an optional port.
    #[error(
        "{value:?} is not a public host: {why}; write a host name such as `puddle.example.org` or `puddle.example.org:8443`"
    )]
    PublicHost {
        /// What was written.
        value: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// A workspace base domain that is not a host name.
    #[error(
        "{value:?} is not a workspace base domain: {why}; write a domain such as `ws.example.org`"
    )]
    BaseDomain {
        /// What was written.
        value: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// Remote serving on with no public host.
    #[error("remote serving needs at least one public host (`public_hosts`)")]
    NoPublicHost,
    /// No base domain is set and the UI's own host is an address, which has no subdomains.
    #[error(
        "the UI's host {host} is an IP address, which has no subdomains; set `workspace_base_domain`"
    )]
    NoSubdomains {
        /// The UI host.
        host: String,
    },
    /// The base domain is one of the public hosts, so a workspace could not be told from the UI.
    #[error("`workspace_base_domain` {domain} is also a public host; use a different domain")]
    BaseIsPublicHost {
        /// The domain.
        domain: String,
    },
    /// A public host that is also a workspace's web-app name: one label below the base domain.
    #[error(
        "the public host {host} is one label below the workspace base domain {domain}, so a workspace of that name would share it; use a host outside the base domain or deeper than one label"
    )]
    PublicHostIsWorkspaceName {
        /// The public host.
        host: String,
        /// The base domain.
        domain: String,
    },
    /// A network listener that would accept nobody.
    #[error("a network listener needs at least one allowed proxy address (`allowed_proxies`)")]
    NoAllowedProxy,
    /// A network listener bound to the unspecified address, which is every interface.
    #[error(
        "the network listener's interface {interface} is the unspecified address, which binds every interface; name the one interface the proxy reaches"
    )]
    UnspecifiedInterface {
        /// The address.
        interface: IpAddr,
    },
    /// An allowed proxy that is the unspecified address, which no peer has.
    #[error(
        "the allowed proxy {proxy} is the unspecified address, which no peer has; name the proxy's own address"
    )]
    UnspecifiedProxy {
        /// The address.
        proxy: IpAddr,
    },
}

/// One exact public authority: a host name or IP address and a port, as the browser shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicHost {
    host: String,
    port: Option<u16>,
}

impl PublicHost {
    /// Parses `host` or `host:port`. The port 443 is the same as none, because a browser does
    /// not send it. Case is folded and one trailing dot dropped.
    ///
    /// # Errors
    ///
    /// [`RemoteError::PublicHost`] for a scheme, a path, a user name, an empty part, a bad port
    /// or a host that is neither a DNS name nor an IP address.
    pub fn parse(text: &str) -> Result<Self, RemoteError> {
        let fail = |why| RemoteError::PublicHost {
            value: text.to_owned(),
            why,
        };
        if text.contains("://") {
            return Err(fail("leave out the scheme"));
        }
        if text.contains(['/', '?', '#', '@', ' ']) {
            return Err(fail("only a host and an optional port"));
        }
        let (host, port) = match text.strip_prefix('[') {
            Some(rest) => {
                let (inside, after) = rest.split_once(']').ok_or_else(|| fail("a missing `]`"))?;
                (format!("[{inside}]"), after.strip_prefix(':'))
            }
            None => match text.split_once(':') {
                Some((host, port)) => (host.to_owned(), Some(port)),
                None => (text.to_owned(), None),
            },
        };
        let port = match port {
            None => None,
            Some(digits) => Some(
                digits
                    .parse::<u16>()
                    .ok()
                    .filter(|p| *p != 0)
                    .ok_or_else(|| fail("the port must be 1 to 65535"))?,
            ),
        };
        let host = host.to_ascii_lowercase();
        let host = host.strip_suffix('.').unwrap_or(&host).to_owned();
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        if bare.parse::<IpAddr>().is_err() {
            DomainName::parse_normalised(&host).map_err(|_| fail("not a valid host name"))?;
        } else if host.starts_with('[') != bare.contains(':') {
            return Err(fail("an IPv6 address goes in brackets"));
        }
        Ok(Self {
            host,
            port: port.filter(|p| *p != 443),
        })
    }

    /// The `Host` header value a browser sends for this authority, lower case.
    #[must_use]
    pub fn authority(&self) -> String {
        match self.port {
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        }
    }

    /// The `Origin` header value of a page served from this authority.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("https://{}", self.authority())
    }

    /// The host without the port.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port the public front listens on: the written one, else 443.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port.unwrap_or(443)
    }
}

impl fmt::Display for PublicHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.authority())
    }
}

/// Remote serving: what a front on this machine, or a proxy elsewhere, may use to reach Puddle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteServing {
    public_hosts: Vec<PublicHost>,
    workspace_names: WorkspaceNames,
}

impl RemoteServing {
    /// Remote serving for these public hosts (the first is the UI's own). `base_domain` is the
    /// domain the workspaces' web apps live under; without one they are subdomains of the UI's
    /// host.
    ///
    /// # Errors
    ///
    /// [`RemoteError`] when there is no public host, the base domain is not a host name or is a
    /// public host, a public host is one label below the base domain (a workspace of that name
    /// would share it), or there is no base domain and the UI's host is an IP address.
    pub fn new(
        public_hosts: Vec<PublicHost>,
        base_domain: Option<&str>,
    ) -> Result<Self, RemoteError> {
        let ui = public_hosts.first().ok_or(RemoteError::NoPublicHost)?;
        let base = match base_domain {
            Some(text) => {
                let text = text.to_ascii_lowercase();
                let name = text.strip_suffix('.').unwrap_or(&text);
                DomainName::parse_normalised(name)
                    .map_err(|_| RemoteError::BaseDomain {
                        value: text.clone(),
                        why: "not a valid host name",
                    })?
                    .as_str()
                    .to_owned()
            }
            None if ui.host().starts_with('[') || ui.host().parse::<IpAddr>().is_ok() => {
                return Err(RemoteError::NoSubdomains {
                    host: ui.host().to_owned(),
                });
            }
            None => ui.host().to_owned(),
        };
        if base_domain.is_some() && public_hosts.iter().any(|h| h.host() == base) {
            return Err(RemoteError::BaseIsPublicHost { domain: base });
        }
        let workspace_names = WorkspaceNames {
            base,
            port: ui.port,
        };
        if let Some(host) = public_hosts
            .iter()
            .find(|h| workspace_names.is_workspace_label_of(h.host()))
        {
            return Err(RemoteError::PublicHostIsWorkspaceName {
                host: host.host().to_owned(),
                domain: workspace_names.base.clone(),
            });
        }
        Ok(Self {
            public_hosts,
            workspace_names,
        })
    }

    /// The public authorities, the UI's own first.
    #[must_use]
    pub fn public_hosts(&self) -> &[PublicHost] {
        &self.public_hosts
    }

    /// The names workspaces' web apps use.
    #[must_use]
    pub fn workspace_names(&self) -> &WorkspaceNames {
        &self.workspace_names
    }
}

/// The names of a workspace's web apps: `<name>.<base domain>`. Puddle needs them to check
/// `Host` and `Origin`, to route a request by its `Host` to a workspace and to build the links it
/// shows. DNS and certificates for them are the reverse proxy's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceNames {
    base: String,
    port: Option<u16>,
}

impl WorkspaceNames {
    /// The domain the names live under.
    #[must_use]
    pub fn base_domain(&self) -> &str {
        &self.base
    }

    /// The `Host` of a workspace's web apps.
    #[must_use]
    pub fn host_for(&self, workspace: &str) -> String {
        let name = format!("{workspace}.{}", self.base);
        match self.port {
            Some(port) => format!("{name}:{port}"),
            None => name,
        }
    }

    /// The `Origin` of a workspace's web apps.
    #[must_use]
    pub fn origin_for(&self, workspace: &str) -> String {
        format!("https://{}", self.host_for(workspace))
    }

    /// The workspace a `Host` header names, when it is `<workspace>.<base domain>` with the
    /// right port and one label in front.
    #[must_use]
    pub fn workspace_of(&self, host: &str) -> Option<String> {
        let host = host.to_ascii_lowercase();
        let name = match self.port {
            Some(port) => host.strip_suffix(&format!(":{port}"))?,
            None => host.as_str(),
        };
        let label = name.strip_suffix(self.base.as_str())?.strip_suffix('.')?;
        (!label.is_empty() && !label.contains('.')).then(|| label.to_owned())
    }

    /// Whether `name` (a DNS name without a port) is one label below the base domain.
    fn is_workspace_label_of(&self, name: &str) -> bool {
        name.strip_suffix(self.base.as_str())
            .and_then(|label| label.strip_suffix('.'))
            .is_some_and(|label| !label.is_empty() && !label.contains('.'))
    }

    /// Whether `name` (a DNS name without a port) is the base domain or below it.
    #[must_use]
    pub fn covers(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name == self.base
            || name
                .strip_suffix(self.base.as_str())
                .is_some_and(|rest| rest.ends_with('.'))
    }
}

/// Everything remote serving turns on: the public names and, optionally, the listener for a
/// reverse proxy on another machine. The listener is a part of it, so a listener without remote
/// serving cannot be written down.
#[derive(Debug, Clone)]
pub struct Remote {
    serving: RemoteServing,
    network: Option<NetworkListener>,
}

impl Remote {
    /// Remote serving for a reverse proxy on this machine.
    #[must_use]
    pub fn new(serving: RemoteServing) -> Self {
        Self {
            serving,
            network: None,
        }
    }

    /// Adds the listener for a reverse proxy on another machine.
    #[must_use]
    pub fn with_network_listener(mut self, network: NetworkListener) -> Self {
        self.network = Some(network);
        self
    }

    /// The public names and the workspace base domain.
    #[must_use]
    pub fn serving(&self) -> &RemoteServing {
        &self.serving
    }

    /// The listener for a reverse proxy on another machine, when there is one.
    #[must_use]
    pub fn network_listener(&self) -> Option<&NetworkListener> {
        self.network.as_ref()
    }
}

/// A certificate and its private key for the network listener's TLS.
#[derive(Clone)]
pub struct TlsIdentity {
    cert_der: Vec<u8>,
    key_pkcs8_der: Zeroizing<Vec<u8>>,
}

impl TlsIdentity {
    /// The certificate (DER) and its private key (PKCS#8 DER).
    #[must_use]
    pub fn new(cert_der: Vec<u8>, key_pkcs8_der: Vec<u8>) -> Self {
        Self {
            cert_der,
            key_pkcs8_der: Zeroizing::new(key_pkcs8_der),
        }
    }

    /// The certificate, DER.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub(crate) fn key_pkcs8_der(&self) -> &[u8] {
        &self.key_pkcs8_der
    }

    /// The SHA-256 of the certificate as lower-case hex: what a proxy pins.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let digest = Sha256::digest(&self.cert_der);
        digest.iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
    }
}

impl fmt::Debug for TlsIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsIdentity")
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// Where the listener for a reverse proxy on another machine binds and whom it admits: one
/// chosen interface and the listed peer addresses (checked at accept, before the handshake).
/// Only [`NetworkEndpoint::new`] makes one, so the unspecified address (every interface, or a
/// peer nobody has) and an empty peer list cannot exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkEndpoint {
    interface: IpAddr,
    port: u16,
    allowed_proxies: Vec<IpAddr>,
}

impl NetworkEndpoint {
    /// The listener's interface, port and the proxy addresses that may connect. A `port` of 0
    /// lets the system pick one.
    ///
    /// # Errors
    ///
    /// [`RemoteError::UnspecifiedInterface`] for `0.0.0.0` or `::` (also as a mapped IPv4
    /// address), [`RemoteError::NoAllowedProxy`] for an empty list and
    /// [`RemoteError::UnspecifiedProxy`] for an unspecified address in it.
    pub fn new(
        interface: IpAddr,
        port: u16,
        allowed_proxies: Vec<IpAddr>,
    ) -> Result<Self, RemoteError> {
        if interface.to_canonical().is_unspecified() {
            return Err(RemoteError::UnspecifiedInterface { interface });
        }
        if allowed_proxies.is_empty() {
            return Err(RemoteError::NoAllowedProxy);
        }
        if let Some(proxy) = allowed_proxies
            .iter()
            .find(|p| p.to_canonical().is_unspecified())
        {
            return Err(RemoteError::UnspecifiedProxy { proxy: *proxy });
        }
        Ok(Self {
            interface,
            port,
            allowed_proxies,
        })
    }

    /// The interface address to bind.
    #[must_use]
    pub fn interface(&self) -> IpAddr {
        self.interface
    }

    /// The port; 0 lets the system pick one.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The proxy addresses that may connect.
    #[must_use]
    pub fn allowed_proxies(&self) -> &[IpAddr] {
        &self.allowed_proxies
    }
}

/// The opt-in listener for a reverse proxy on another machine: a valid [`NetworkEndpoint`] and
/// the certificate it presents over TLS.
#[derive(Debug, Clone)]
pub struct NetworkListener {
    endpoint: NetworkEndpoint,
    tls: TlsIdentity,
}

impl NetworkListener {
    /// The listener for `endpoint`, presenting `tls`.
    #[must_use]
    pub fn new(endpoint: NetworkEndpoint, tls: TlsIdentity) -> Self {
        Self { endpoint, tls }
    }

    /// Where it binds and whom it admits.
    #[must_use]
    pub fn endpoint(&self) -> &NetworkEndpoint {
        &self.endpoint
    }

    /// The certificate it presents.
    #[must_use]
    pub fn tls(&self) -> &TlsIdentity {
        &self.tls
    }
}

#[cfg(test)]
mod tests;
