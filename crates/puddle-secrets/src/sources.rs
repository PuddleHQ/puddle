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
    let text = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    // The account is in no such tenant, or the tenant does not exist.
    if has(&["aadsts90002", "aadsts50020", "aadsts90072", "aadsts700016"])
        || (text.contains("tenant")
            && has(&[
                "not found",
                "not a member",
                "doesn't exist",
                "does not exist",
            ]))
    {
        return SourceError::WrongTenant;
    }
    // Signed in, but Azure DevOps is not open to this account in this tenant.
    if has(&[
        "aadsts65001",
        "aadsts50105",
        "aadsts500011",
        "aadsts650057",
        "aadsts53003",
    ]) || has(&["does not have access", "not authorized to access"])
    {
        return SourceError::NoAccess;
    }
    // No login, or one that must be redone (expired refresh token, MFA, conditional access).
    if has(&[
        "az login",
        "no accounts",
        "interactive authentication is needed",
        "refresh token has expired",
        "aadsts50173",
        "aadsts50076",
        "aadsts50079",
        "aadsts70043",
        "aadsts70008",
        "aadsts700082",
        "aadsts50078",
        "aadsts50132",
        "aadsts50133",
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
mod tests {
    use super::*;

    fn target() -> (HostName, UrlPath) {
        (
            HostName::new("dev.azure.com").unwrap(),
            UrlPath::new("org").unwrap(),
        )
    }

    #[test]
    fn fill_answer_must_match_the_request() {
        let (h, p) = target();
        let ok = "protocol=https\nhost=dev.azure.com\npath=org\nusername=me\npassword=CANARY-1\n";
        let got = parse_fill(ok, &h, &p, None).unwrap();
        assert_eq!(got.credential.secret.expose(), "CANARY-1");
        assert_eq!(got.credential.username.as_deref(), Some("me"));
        for bad in [
            "protocol=https\nhost=github.com\npath=org\npassword=x\n",
            "protocol=https\nhost=dev.azure.com\npath=other\npassword=x\n",
            "protocol=https\nhost=dev.azure.com\npassword=x\n",
            "protocol=http\nhost=dev.azure.com\npath=org\npassword=x\n",
            "password=x\n",
        ] {
            assert_eq!(
                parse_fill(bad, &h, &p, None).unwrap_err(),
                SourceError::Refused(Refusal::WrongTarget),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn fill_answer_checks_the_account_and_the_password() {
        let (h, p) = target();
        let me = AccountName::new("me").unwrap();
        let other = "host=dev.azure.com\npath=org\nusername=you\npassword=x\n";
        assert!(parse_fill(other, &h, &p, Some(&me)).is_err());
        let none = "host=dev.azure.com\npath=org/\nusername=me\npassword=\n";
        assert_eq!(
            parse_fill(none, &h, &p, Some(&me)).unwrap_err(),
            SourceError::NotSignedIn
        );
        let spaced = "host=DEV.azure.com\npath=org\npassword=a b\n";
        assert!(parse_fill(spaced, &h, &p, None).is_ok());
        let ctrl = "host=dev.azure.com\npath=org\npassword=a\u{7}b\n";
        assert_eq!(
            parse_fill(ctrl, &h, &p, None).unwrap_err(),
            SourceError::Refused(Refusal::Malformed)
        );
    }

    #[test]
    fn fill_answer_expiry() {
        let (h, p) = target();
        // The remaining time is checked on the pure function, with a fixed clock: it has no secret in
        // reach and no dependence on the machine's speed.
        assert_eq!(remaining_validity(None, 1000), Ok(None));
        assert_eq!(
            remaining_validity(Some(4600), 1000),
            Ok(Some(Duration::from_secs(3600)))
        );
        assert_eq!(
            remaining_validity(Some(1001), 1000),
            Ok(Some(Duration::from_secs(1)))
        );
        assert_eq!(
            remaining_validity(Some(1000), 1000),
            Err(SourceError::NotSignedIn)
        );
        let live = "host=dev.azure.com\npath=org\npassword=x\npassword_expiry_utc=99999999999\n";
        assert!(parse_fill(live, &h, &p, None).unwrap().valid_for.is_some());
        let dead = "host=dev.azure.com\npath=org\npassword=x\npassword_expiry_utc=5\n";
        assert_eq!(
            parse_fill(dead, &h, &p, None).unwrap_err(),
            SourceError::NotSignedIn
        );
    }

    const GUID: &str = "72f988bf-86f1-41af-91ab-2d7cd011db47";

    fn tenant(s: &str) -> TenantId {
        TenantId::new(s).unwrap()
    }

    #[test]
    fn az_answer_is_read_field_by_field() {
        let json = format!(
            r#"{{"accessToken":"CANARY-az","expiresOn":"2026-11-06 12:00:00.000000","expires_on":1000100,"subscription":"x","tenant":"{}","tokenType":"Bearer"}}"#,
            GUID.to_uppercase()
        );
        let got = parse_az_token(json.as_bytes(), &tenant(GUID), 1_000_000).unwrap();
        assert_eq!(got.credential.secret.expose(), "CANARY-az");
        assert_eq!(got.valid_for, Some(Duration::from_secs(100)));
        // A string epoch (older az) and a missing one (the cache limit applies) both work.
        let text = r#"{"accessToken":"a","expires_on":"1000100","tenant":"t"}"#;
        assert!(
            parse_az_token(text.as_bytes(), &tenant("t.io"), 1_000_000)
                .unwrap()
                .valid_for
                .is_some()
        );
        // An `expires_on` of another type counts as none.
        let odd = r#"{"accessToken":"a","expires_on":true}"#;
        assert_eq!(
            parse_az_token(odd.as_bytes(), &tenant("t.io"), 0)
                .unwrap()
                .valid_for,
            None
        );
        let none = r#"{"accessToken":"a","expiresOn":"2026-11-06 12:00:00.000000"}"#;
        assert_eq!(
            parse_az_token(none.as_bytes(), &tenant("t.io"), 0)
                .unwrap()
                .valid_for,
            None
        );
    }

    #[test]
    fn unix_now_is_the_real_clock() {
        let real = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let got = unix_now();
        assert!(got >= 1_700_000_000, "{got}");
        assert!(got.abs_diff(real) <= 5, "{got} vs {real}");
    }

    #[test]
    fn guid_needs_five_hex_parts_of_the_right_lengths() {
        assert!(is_guid("11111111-2222-3333-4444-555555555555"));
        assert!(is_guid("abcdef01-ABCD-ef01-2345-6789abcdef01"));
        for bad in [
            // Right length and dash count, but not hexadecimal.
            "gggggggg-2222-3333-4444-555555555555",
            "11111111-2222-3333-4444-55555555555z",
            // Right characters and part lengths, but too few or too many parts.
            "11111111-2222-3333-4444",
            "11111111-2222-3333-4444-555555555555-6666",
            // Dashes in the wrong places (hex only, same length).
            "1111111-12222-3333-4444-555555555555",
            "11111111-2222-333-34444-555555555555",
            "11111111-2222-3333-4444-55555555555-5",
            // A domain name, and the empty string.
            "contoso.onmicrosoft.com",
            "",
        ] {
            assert!(!is_guid(bad), "{bad}");
        }
    }

    #[test]
    fn az_answer_for_another_tenant_or_unreadable_is_refused() {
        let other = r#"{"accessToken":"a","expires_on":9999999999,"tenant":"11111111-2222-3333-4444-555555555555"}"#;
        assert_eq!(
            parse_az_token(other.as_bytes(), &tenant(GUID), 0).unwrap_err(),
            SourceError::WrongTenant
        );
        let missing = r#"{"accessToken":"a","expires_on":9999999999}"#;
        assert_eq!(
            parse_az_token(missing.as_bytes(), &tenant(GUID), 0).unwrap_err(),
            SourceError::WrongTenant
        );
        for (bad, why) in [
            ("not json", Refusal::Unreadable),
            ("{}", Refusal::Unreadable),
            (r#"{"accessToken":5}"#, Refusal::Unreadable),
            (r#"{"accessToken":"a b"}"#, Refusal::Malformed),
            (r#"{"accessToken":"a\nb"}"#, Refusal::Malformed),
        ] {
            assert_eq!(
                parse_az_token(bad.as_bytes(), &tenant("t.io"), 0).unwrap_err(),
                SourceError::Refused(why),
                "{bad}"
            );
        }
        let past = r#"{"accessToken":"a","expires_on":5}"#;
        assert_eq!(
            parse_az_token(past.as_bytes(), &tenant("t.io"), 10).unwrap_err(),
            SourceError::NotSignedIn
        );
    }

    #[test]
    fn az_failures_name_the_missing_piece() {
        for (stderr, want) in [
            (
                "ERROR: Please run 'az login' to setup account.",
                SourceError::NotSignedIn,
            ),
            ("ERROR: No accounts were found", SourceError::NotSignedIn),
            (
                "AADSTS70043: The refresh token has expired",
                SourceError::NotSignedIn,
            ),
            (
                "AADSTS50076: MFA needed. Interactive authentication is needed.",
                SourceError::NotSignedIn,
            ),
            (
                "AADSTS90002: Tenant 'x' not found.",
                SourceError::WrongTenant,
            ),
            (
                "AADSTS50020: User account from another identity provider",
                SourceError::WrongTenant,
            ),
            (
                "ERROR: Tenant 'abc' is not found in the account",
                SourceError::WrongTenant,
            ),
            (
                "AADSTS65001: The user has not consented",
                SourceError::NoAccess,
            ),
            (
                "AADSTS500011: resource principal not found",
                SourceError::NoAccess,
            ),
            (
                "Traceback (most recent call last): boom",
                SourceError::CouldNotRun(Tool::Az),
            ),
            ("", SourceError::CouldNotRun(Tool::Az)),
        ] {
            assert_eq!(classify_az_failure(stderr), want, "{stderr}");
        }
    }

    #[test]
    fn pasted_tokens_are_trimmed_and_checked() {
        assert_eq!(pasted_token("  abc\n").unwrap().expose(), "abc");
        assert!(pasted_token("   ").is_err());
        assert!(pasted_token("a b").is_err());
        assert!(pasted_token("a\nb").is_err());
        assert!(pasted_token(&"a".repeat(9000)).is_err());
    }

    #[test]
    fn a_token_of_8192_bytes_is_taken_and_one_byte_more_is_not() {
        assert_eq!(
            token_secret(&"a".repeat(8192), false)
                .unwrap()
                .expose()
                .len(),
            8192
        );
        assert_eq!(
            token_secret(&"a".repeat(8193), false).unwrap_err(),
            SourceError::Refused(Refusal::Malformed)
        );
    }
}
