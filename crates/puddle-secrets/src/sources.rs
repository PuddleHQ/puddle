// SPDX-License-Identifier: GPL-3.0-or-later
//! The sources behind one interface: [`Fetch`].

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::error::{Refusal, SourceError, Tool};
use crate::name::{AccountName, HostName, StoredId, TenantId, UrlPath};
use crate::run::{AZ_TIMEOUT, TOOL_TIMEOUT, ToolPaths, run, run_capturing};
use crate::secret::Secret;
use crate::spec::SourceSpec;
use crate::store::SecretStore;

/// A credential read from a source. Cheap to clone: the secret is shared and wiped when the last
/// holder drops it.
///
/// The same value serves the proxy (which puts it in a header) and host-side API calls (which
/// send it to the Git host's own API); whoever uses it checks [`SourceSpec::scope`] first.
#[derive(Debug, Clone)]
pub struct Credential {
    /// The user name the source reported, when it has one (Git Credential Manager does).
    pub username: Option<String>,
    /// The token.
    pub secret: Arc<Secret>,
}

/// A credential and how long its source says it stays valid.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// The credential.
    pub credential: Credential,
    /// How long from now the credential is good, when the source knows (an Entra token does).
    pub valid_for: Option<Duration>,
}

/// One interface over every source: give a [`SourceSpec`], get a credential or a typed reason. It
/// never asks the user anything; a source that needs a login answers
/// [`SourceError::NotSignedIn`].
pub trait Fetch: Send + Sync {
    /// Reads the secret `spec` names.
    fn fetch(&self, spec: &SourceSpec)
    -> impl Future<Output = Result<Fetched, SourceError>> + Send;
}

/// Azure DevOps' own application id: the resource an Azure DevOps token is asked for.
const AZURE_DEVOPS_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

/// The real sources: `gh`, `git credential fill`, `az` and the OS store.
pub struct Sources {
    tools: ToolPaths,
    store: Arc<dyn SecretStore>,
    timeout: Duration,
    az_timeout: Duration,
}

impl Sources {
    /// Sources that run the tools at `tools` and read pasted tokens from `store`.
    #[must_use]
    pub fn new(tools: ToolPaths, store: Arc<dyn SecretStore>) -> Self {
        Self {
            tools,
            store,
            timeout: TOOL_TIMEOUT,
            az_timeout: AZ_TIMEOUT,
        }
    }

    /// The same sources with another time limit per tool call (the default is 10 seconds, and 20
    /// for `az`, whose cold start is slow).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self.az_timeout = timeout;
        self
    }

    /// Reads an Entra token for Azure DevOps from the account signed in to `az` for `tenant`.
    /// `az` answers from its own login cache; nothing here reads or writes that cache, and
    /// nothing starts a sign-in.
    async fn az_cli(&self, tenant: &TenantId) -> Result<Fetched, SourceError> {
        let program = self.tools.get(Tool::Az)?;
        let args = [
            "account",
            "get-access-token",
            "--resource",
            AZURE_DEVOPS_RESOURCE,
            "--tenant",
            tenant.as_str(),
            "--output",
            "json",
        ];
        let out = run_capturing(Tool::Az, program, &args, self.az_timeout).await?;
        if !out.success {
            let err = classify_az_failure(&String::from_utf8_lossy(&out.stderr));
            tracing::debug!(error = %err, "az could not give a token");
            return Err(err);
        }
        parse_az_token(&out.stdout, tenant, unix_now())
    }

    /// Reads the token of one `gh` account. `gh auth token --user` answers from the keyring
    /// without a prompt, also for an account that is not the active one.
    async fn gh(&self, host: &HostName, account: &AccountName) -> Result<Fetched, SourceError> {
        let program = self.tools.get(Tool::Gh)?;
        let args = [
            "auth",
            "token",
            "--hostname",
            host.as_str(),
            "--user",
            account.as_str(),
        ];
        let out = run(Tool::Gh, program, &args, None, self.timeout).await?;
        if !out.success {
            return Err(SourceError::NotSignedIn);
        }
        let text = std::str::from_utf8(&out.stdout)
            .map_err(|_| SourceError::Refused(Refusal::Unreadable))?
            .trim();
        if text.is_empty() {
            return Err(SourceError::NotSignedIn);
        }
        let secret = token_secret(text, false)?;
        Ok(Fetched {
            credential: Credential {
                username: None,
                secret,
            },
            valid_for: None,
        })
    }

    /// Asks Git for the credential of one URL with its path, with every prompt off.
    async fn git_credential(
        &self,
        host: &HostName,
        path: &UrlPath,
        username: Option<&AccountName>,
    ) -> Result<Fetched, SourceError> {
        let program = self.tools.get(Tool::Git)?;
        // `useHttpPath` makes Git keep `path` in the request; without it Git drops the path and the
        // helper may answer from another organisation's entry.
        let args = ["-c", "credential.useHttpPath=true", "credential", "fill"];
        let mut input = format!("protocol=https\nhost={host}\npath={path}\n");
        if let Some(user) = username {
            input.push_str("username=");
            input.push_str(user.as_str());
            input.push('\n');
        }
        input.push('\n');
        let out = run(
            Tool::Git,
            program,
            &args,
            Some(input.as_bytes()),
            self.timeout,
        )
        .await?;
        if !out.success {
            return Err(SourceError::NotSignedIn);
        }
        let text = std::str::from_utf8(&out.stdout)
            .map_err(|_| SourceError::Refused(Refusal::Unreadable))?;
        parse_fill(text, host, path, username)
    }

    async fn stored(&self, id: &StoredId) -> Result<Fetched, SourceError> {
        let store = Arc::clone(&self.store);
        let id = id.clone();
        let found = tokio::task::spawn_blocking(move || store.get(&id))
            .await
            .map_err(|_| SourceError::StoreUnavailable)?
            .map_err(|_| SourceError::StoreUnavailable)?;
        let secret = found.ok_or(SourceError::NotSignedIn)?;
        Ok(Fetched {
            credential: Credential {
                username: None,
                secret: Arc::new(secret),
            },
            valid_for: None,
        })
    }
}

impl Fetch for Sources {
    async fn fetch(&self, spec: &SourceSpec) -> Result<Fetched, SourceError> {
        tracing::debug!(source = %spec.describe(), "reading a secret");
        match spec {
            SourceSpec::Gh { host, account } => self.gh(host, account).await,
            SourceSpec::GitCredential {
                host,
                path,
                username,
            } => self.git_credential(host, path, username.as_ref()).await,
            SourceSpec::Stored { id, .. } => self.stored(id).await,
            SourceSpec::AzCli { tenant, .. } => self.az_cli(tenant).await,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Sorts a failed `az account get-access-token` by what it said on standard error, so the user is
/// told which piece is missing. The text is read here and dropped: it is never logged or sent.
pub(crate) fn classify_az_failure(stderr: &str) -> SourceError {
    // An `AADSTS` code says exactly what failed, so it decides before any wording does: the
    // message of a code often uses the words of another case ("was not found in the tenant").
    // The account is in no such tenant, or the tenant does not exist.
    const WRONG_TENANT: &[&str] = &["aadsts90002", "aadsts50020", "aadsts90072", "aadsts700016"];
    // Signed in, but Azure DevOps is not open to this account in this tenant.
    const NO_ACCESS: &[&str] = &[
        "aadsts65001",
        "aadsts50105",
        "aadsts500011",
        "aadsts650057",
        "aadsts53003",
    ];
    // No login, or one that must be redone (expired refresh token, MFA, conditional access).
    const NOT_SIGNED_IN: &[&str] = &[
        "aadsts50173",
        "aadsts50076",
        "aadsts50079",
        "aadsts70043",
        "aadsts70008",
        "aadsts700082",
        "aadsts50078",
        "aadsts50132",
        "aadsts50133",
    ];
    let text = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    for (codes, error) in [
        (WRONG_TENANT, SourceError::WrongTenant),
        (NO_ACCESS, SourceError::NoAccess),
        (NOT_SIGNED_IN, SourceError::NotSignedIn),
    ] {
        if has(codes) {
            return error;
        }
    }
    if text.contains("tenant")
        && has(&[
            "not found",
            "not a member",
            "doesn't exist",
            "does not exist",
        ])
    {
        return SourceError::WrongTenant;
    }
    if has(&["does not have access", "not authorized to access"]) {
        return SourceError::NoAccess;
    }
    if has(&[
        "az login",
        "no accounts",
        "interactive authentication is needed",
        "refresh token has expired",
    ]) {
        return SourceError::NotSignedIn;
    }
    SourceError::CouldNotRun(Tool::Az)
}

/// Reads the answer of `az account get-access-token`: three fields, nothing else kept. The token
/// is used only for the tenant that was asked for, when that is a GUID (a domain name is resolved
/// by `az`, which answers with the GUID, so it cannot be compared).
pub(crate) fn parse_az_token(
    stdout: &[u8],
    tenant: &TenantId,
    now: u64,
) -> Result<Fetched, SourceError> {
    let unreadable = || SourceError::Refused(Refusal::Unreadable);
    let mut doc: serde_json::Value = serde_json::from_slice(stdout).map_err(|_| unreadable())?;
    let token = match doc.get_mut("accessToken").map(serde_json::Value::take) {
        Some(serde_json::Value::String(s)) => zeroize::Zeroizing::new(s),
        _ => return Err(unreadable()),
    };
    if is_guid(tenant.as_str()) {
        let answered = doc.get("tenant").and_then(serde_json::Value::as_str);
        if !answered.is_some_and(|t| t.eq_ignore_ascii_case(tenant.as_str())) {
            return Err(SourceError::WrongTenant);
        }
    }
    // `expires_on` is epoch seconds (a number, or a string on older versions). `expiresOn` is local
    // time and has no zone, so it is not read: the cache's 55 minute limit covers an answer
    // without `expires_on`.
    let expiry = doc.get("expires_on").and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    });
    let valid_for = remaining_validity(expiry, now)?;
    let secret = token_secret(token.trim(), false)?;
    Ok(Fetched {
        credential: Credential {
            username: None,
            secret,
        },
        valid_for,
    })
}

/// `8-4-4-4-12` hexadecimal digits.
fn is_guid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Checks a token's characters and wraps it. `allow_space` is for helper passwords, which are not
/// always tokens; control characters never pass (they would end up in a header).
fn token_secret(text: &str, allow_space: bool) -> Result<Arc<Secret>, SourceError> {
    let bad = |c: char| c.is_control() || (!allow_space && c.is_whitespace());
    if text.len() > 8192 || text.chars().any(bad) {
        return Err(SourceError::Refused(Refusal::Malformed));
    }
    Ok(Arc::new(Secret::new(text.to_owned())))
}

/// Checks a token the user pasted before it is stored: trims surrounding whitespace and refuses
/// an empty value, inner whitespace or control characters.
///
/// # Errors
/// [`SourceError::Refused`] with [`Refusal::Malformed`].
pub fn pasted_token(text: &str) -> Result<Secret, SourceError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(SourceError::Refused(Refusal::Malformed));
    }
    let arc = token_secret(text, false)?;
    Ok(Secret::new(arc.expose().to_owned()))
}

/// Reads `git credential fill` output. The answer is used only when it names the host and path
/// that were asked for (and the account, when one was): a helper that answered for another entry
/// is the failure this guards against.
pub(crate) fn parse_fill(
    text: &str,
    host: &HostName,
    path: &UrlPath,
    username: Option<&AccountName>,
) -> Result<Fetched, SourceError> {
    let (mut got_host, mut got_path, mut got_user) = (None, None, None);
    let (mut protocol, mut password, mut expiry) = (None, None, None);
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "protocol" => protocol = Some(value),
            "host" => got_host = Some(value),
            "path" => got_path = Some(value),
            "username" => got_user = Some(value),
            "password" => password = Some(value),
            "password_expiry_utc" => expiry = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    let wrong = SourceError::Refused(Refusal::WrongTarget);
    if protocol.is_some_and(|p| p != "https") {
        return Err(wrong);
    }
    if !got_host.is_some_and(|h| h.eq_ignore_ascii_case(host.as_str())) {
        return Err(wrong);
    }
    if !got_path.is_some_and(|p| p.trim_matches('/').eq_ignore_ascii_case(path.as_str())) {
        return Err(wrong);
    }
    if let Some(asked) = username
        && !got_user.is_some_and(|u| u.eq_ignore_ascii_case(asked.as_str()))
    {
        return Err(wrong);
    }
    let password = password
        .filter(|p| !p.is_empty())
        .ok_or(SourceError::NotSignedIn)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let valid_for = remaining_validity(expiry, now)?;
    let secret = token_secret(password, true)?;
    Ok(Fetched {
        credential: Credential {
            username: got_user.map(str::to_owned),
            secret,
        },
        valid_for,
    })
}

/// Time left until `expiry` (Unix seconds), `None` when the answer states none; an expiry at or before
/// `now` means the sign-in has lapsed.
fn remaining_validity(expiry: Option<u64>, now: u64) -> Result<Option<Duration>, SourceError> {
    match expiry {
        None => Ok(None),
        Some(at) if at <= now => Err(SourceError::NotSignedIn),
        Some(at) => Ok(Some(Duration::from_secs(at - now))),
    }
}

#[cfg(test)]
mod tests;
