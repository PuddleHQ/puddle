// SPDX-License-Identifier: GPL-3.0-or-later
//! The HTTPS leg of Git over SSH: Puddle reaches the Git host as the workspace's own `git` would
//! for the HTTPS remote.
//!
//! Each command opens one HTTPS connection to the repository's host through the workspace's own
//! TLS termination, in memory, as a client that trusts the workspace's CA. So everything an HTTPS
//! request gets, this one gets the same way: the rules, the repository table, the identity's
//! credential added by the injector, a refused credential's own message, the request rules, the
//! company-proxy route and the check of the host's certificate. The guest never sees a key or a
//! token, and nothing it receives could be replayed.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use http_body_util::BodyExt as _;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use puddle_types::{EgressRequest, Host};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt as _, BufReader};
use tokio::task::JoinHandle;

use super::bridge::{self, SmartHttp};
use super::{
    GitSshAccount, GitSshAccounts, GitSshCommand, GitSshContext, GitSshFailure, GitSshGreeting,
    GitSshStream, GitSshUpstream, Reached,
};
use crate::counted::Counted;
use crate::destination::BoxFuture;
use crate::proxy::admit;
use crate::target::Target;
use crate::terminate::h1::{BoxError, UpBody};
use crate::terminate::{self, Outcome};

/// The in-memory connection's buffer each way.
const PIPE: usize = 262_144;

/// Most bytes of the termination's `CONNECT` answer read before giving up on it.
const MAX_ANSWER: usize = 1024;

/// How long a finished command waits for its connection's record (the address reached upstream)
/// before it ends without it: the connection closes as soon as the client side drops, so this
/// only bounds a termination that is stuck.
const OUTCOME_WAIT: Duration = Duration::from_secs(5);

/// Git over SSH through the hosts' HTTPS: the default way Puddle reaches a Git host (see the
/// [module](crate::ssh_git) for the guest's side). `accounts` names the account `ssh -T` greets.
#[derive(Debug)]
pub struct GitSshHttps {
    accounts: Arc<dyn GitSshAccounts>,
}

impl GitSshHttps {
    /// The HTTPS leg, greeting `ssh -T` with the account `accounts` names.
    #[must_use]
    pub fn new(accounts: Arc<dyn GitSshAccounts>) -> Self {
        Self { accounts }
    }
}

impl GitSshUpstream for GitSshHttps {
    fn run<'a>(
        &'a self,
        cx: &'a GitSshContext,
        command: &'a GitSshCommand,
        io: &'a mut dyn GitSshStream,
    ) -> BoxFuture<'a, Result<(), GitSshFailure>> {
        Box::pin(async move {
            let mut leg = Leg {
                cx,
                host: command.host.clone(),
                open: None,
            };
            let result = bridge::carry(&mut leg, command, io).await;
            leg.close().await;
            result
        })
    }

    fn greeting<'a>(
        &'a self,
        cx: &'a GitSshContext,
        host: &'a Host,
    ) -> BoxFuture<'a, GitSshGreeting> {
        Box::pin(async move {
            let account = self.accounts.account(cx.workspace(), host).await;
            greeting(host, &account)
        })
    }
}

/// What `ssh -T` says for `account` on `host`: the host's own greeting where Puddle knows it
/// (GitHub, GitLab), the GitHub form for any other host.
pub(crate) fn greeting(host: &Host, account: &GitSshAccount) -> GitSshGreeting {
    let name = match account {
        GitSshAccount::NoIdentity => return super::no_identity(host),
        GitSshAccount::Named(name) if printable(name) => Some(name.as_str()),
        GitSshAccount::Named(_) | GitSshAccount::Unnamed => None,
    };
    let host_name = host.to_string();
    if host_name == "gitlab.com" {
        return GitSshGreeting {
            message: name.map_or_else(
                || "Welcome to GitLab!\n".to_owned(),
                |name| format!("Welcome to GitLab, @{name}!\n"),
            ),
            status: 0,
        };
    }
    let product = if host_name == "github.com" {
        "GitHub"
    } else {
        host_name.as_str()
    };
    let hi = name.map_or_else(|| "Hi!".to_owned(), |name| format!("Hi {name}!"));
    GitSshGreeting {
        message: format!(
            "{hi} You've successfully authenticated, but {product} does not provide shell access.\n"
        ),
        status: 1,
    }
}

/// An account name as Git hosts spell one, so nothing else reaches the guest's terminal.
fn printable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// One command's way to the host: an HTTP/1.1 connection into the termination, opened at the
/// first request.
struct Leg<'a> {
    cx: &'a GitSshContext,
    host: Host,
    open: Option<Open>,
}

struct Open {
    send: SendRequest<UpBody>,
    done: JoinHandle<Outcome>,
}

impl Leg<'_> {
    async fn sender(&mut self) -> Result<&mut SendRequest<UpBody>, GitSshFailure> {
        if self.open.is_none() {
            self.open = Some(self.connect().await?);
        }
        let open = self.open.as_mut().ok_or_else(closed)?;
        open.send.ready().await.map_err(|_| closed())?;
        Ok(&mut open.send)
    }

    async fn connect(&self) -> Result<Open, GitSshFailure> {
        let handler = &self.cx.handler;
        let proxy = &handler.proxy;
        let https = EgressRequest::new(handler.workspace.clone(), self.host.clone(), 443);
        let (admitted, _) = admit(proxy, &https).await;
        let admitted = admitted.map_err(|refusal| GitSshFailure::new(refusal.message))?;
        let (termination, mode, tls) = proxy.terminating(&https, false).ok_or_else(|| {
            GitSshFailure::new(format!(
                "Puddle does not decrypt {} for this workspace, so it cannot carry Git over SSH there",
                self.host
            ))
        })?;
        let root = termination.ca().certificate().der().clone();
        let context = Arc::new(terminate::Context {
            proxy: Arc::clone(proxy),
            termination,
            mode,
            tls: tls.clone(),
            workspace: handler.workspace.clone(),
            target: Target {
                host: self.host.clone(),
                port: 443,
            },
            admitted,
            connections: Arc::clone(&handler.streams),
            streams: Arc::clone(&handler.h2_streams),
        });
        let (mut client, server) = tokio::io::duplex(PIPE);
        let (server, _) = Counted::new(server);
        let done = tokio::spawn(terminate::run(context, BufReader::new(server)));
        connected(&mut client).await?;
        let tls = tls_connector(root, &self.host)?
            .connect(server_name(&self.host)?, client)
            .await
            .map_err(|err| failed("connection to", &self.host, &err))?;
        let (send, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|err| failed("connection to", &self.host, &err))?;
        tokio::spawn(connection);
        Ok(Open { send, done })
    }

    /// Ends the connection and keeps where it went for the connection's record.
    async fn close(self) {
        let Some(open) = self.open else {
            return;
        };
        drop(open.send);
        if let Ok(Ok(outcome)) = tokio::time::timeout(OUTCOME_WAIT, open.done).await {
            self.cx.reached(Reached {
                ip: outcome.resolved_ip,
                hop: outcome.hop,
            });
        }
    }
}

impl SmartHttp for Leg<'_> {
    fn send(
        &mut self,
        request: http::Request<UpBody>,
    ) -> BoxFuture<'_, Result<http::Response<UpBody>, GitSshFailure>> {
        Box::pin(async move {
            let host = self.host.clone();
            let answer = self
                .sender()
                .await?
                .send_request(request)
                .await
                .map_err(|err| failed("request to", &host, &err))?;
            Ok(answer.map(|body| body.map_err(BoxError::from).boxed_unsync()))
        })
    }
}

/// `the <what> <host> failed: <err>`.
fn failed(what: &str, host: &Host, err: &dyn std::fmt::Display) -> GitSshFailure {
    GitSshFailure::new(format!("the {what} {host} failed: {err}"))
}

fn closed() -> GitSshFailure {
    GitSshFailure::new("the connection to the Git host closed")
}

/// Reads the termination's answer to the in-memory `CONNECT`, which is always a `200`.
async fn connected<R: AsyncRead + Unpin>(client: &mut R) -> Result<(), GitSshFailure> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_ANSWER || client.read(&mut byte).await.unwrap_or(0) == 0 {
            return Err(closed());
        }
        head.extend_from_slice(&byte);
    }
    Ok(())
}

fn server_name(host: &Host) -> Result<ServerName<'static>, GitSshFailure> {
    ServerName::try_from(host.to_string())
        .map_err(|_| GitSshFailure::new(format!("{host} is not a name TLS can check")))
}

/// A TLS client that trusts only `root`, the workspace's CA, and speaks HTTP/1.1.
fn tls_connector(
    root: CertificateDer<'static>,
    host: &Host,
) -> Result<tokio_rustls::TlsConnector, GitSshFailure> {
    static PROVIDER: OnceLock<Arc<rustls::crypto::CryptoProvider>> = OnceLock::new();
    let provider =
        Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())));
    let unusable = |err: rustls::Error| failed("TLS set-up for", host, &err);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(root).map_err(unusable)?;
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(unusable)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![terminate::ALPN_HTTP11.to_vec()];
    Ok(tokio_rustls::TlsConnector::from(Arc::new(config)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str) -> Host {
        Host::parse_normalised(name).unwrap_or_else(|_| unreachable!("a valid test name"))
    }

    fn named(name: &str) -> GitSshAccount {
        GitSshAccount::Named(name.to_owned())
    }

    #[tokio::test]
    async fn the_answer_to_the_in_memory_connect_is_read_to_its_blank_line_and_no_further() {
        let mut answer = &b"HTTP/1.1 200 Connection Established\r\n\r\nTLS"[..];
        assert!(connected(&mut answer).await.is_ok());
        assert_eq!(answer, b"TLS");
        for bad in [&b""[..], b"HTTP/1.1 200 OK\r\n"] {
            let mut answer = bad;
            assert_eq!(
                connected(&mut answer).await.unwrap_err().message,
                "the connection to the Git host closed"
            );
        }
        let endless = vec![b'x'; MAX_ANSWER + 10];
        let mut answer = &endless[..];
        assert!(connected(&mut answer).await.is_err());
        assert_eq!(answer.len(), 10, "it stops at its limit");
    }

    #[test]
    fn a_failure_on_the_way_names_the_step_and_the_host() {
        assert_eq!(
            failed("request to", &host("github.com"), &"reset").message,
            "the request to github.com failed: reset"
        );
    }

    #[test]
    fn github_greets_by_name_with_its_own_words_and_status() {
        let said = greeting(&host("github.com"), &named("octocat"));
        assert_eq!(
            said.message,
            "Hi octocat! You've successfully authenticated, but GitHub does not provide shell access.\n"
        );
        assert_eq!(said.status, 1);
    }

    #[test]
    fn gitlab_welcomes_with_status_0() {
        let said = greeting(&host("gitlab.com"), &named("octo"));
        assert_eq!(said.message, "Welcome to GitLab, @octo!\n");
        assert_eq!(said.status, 0);
        let said = greeting(&host("gitlab.com"), &GitSshAccount::Unnamed);
        assert_eq!(said.message, "Welcome to GitLab!\n");
        assert_eq!(said.status, 0);
    }

    #[test]
    fn any_other_host_gets_the_github_form_with_its_name() {
        let said = greeting(&host("git.example.org"), &GitSshAccount::Unnamed);
        assert_eq!(
            said.message,
            "Hi! You've successfully authenticated, but git.example.org does not provide shell access.\n"
        );
        assert_eq!(said.status, 1);
    }

    #[test]
    fn no_identity_is_the_failed_login_line() {
        let said = greeting(&host("github.com"), &GitSshAccount::NoIdentity);
        assert_eq!(said.status, 255);
        assert!(said.message.starts_with("puddle: no identity"));
    }

    #[test]
    fn a_name_that_is_not_one_a_host_spells_is_left_out() {
        for bad in ["", "evil\u{1b}[2J", "two words", &"a".repeat(101)] {
            let said = greeting(&host("github.com"), &named(bad));
            assert!(said.message.starts_with("Hi! "), "{bad:?}");
        }
        for good in ["a", "octo-cat_1.x", &"a".repeat(100)] {
            let said = greeting(&host("github.com"), &named(good));
            assert!(
                said.message.starts_with(&format!("Hi {good}! ")),
                "{good:?}"
            );
        }
    }
}
