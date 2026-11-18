// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain HTTPS reads of public GitHub data: release assets and API answers, fetched with `curl`.
//!
//! No `gh` and no sign-in: the forks and upstream are public, so a host that has only `git` and
//! `curl` (a fresh server, a CI runner) can build the runtime folder. Callers verify what they
//! download against a checksum list; this module only moves bytes.
//!
//! API calls send the token of `GH_TOKEN` (or `GITHUB_TOKEN`) when there is one: a CI runner's
//! shared address has no budget left for anonymous calls (a 403), while the job's token has its
//! own. A host with no token still works (anonymous), except in CI, where a missing token is an
//! error that names the variable. A 403 or 429 is retried with backoff, and the final message
//! carries the rate-limit headers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::{Result, XtaskError};
use crate::tools::{run, run_with_stdin};

/// Tries per API call (the first plus the retries after a 403 or 429).
const API_TRIES: u32 = 4;
/// The longest a `Retry-After` or rate-limit reset is waited for in one go.
const MAX_WAIT: Duration = Duration::from_secs(90);

/// Where to read from and which `curl` to run.
#[derive(Debug, Clone)]
pub struct GitHub {
    /// The REST API root, without a trailing slash.
    api: String,
    /// The web root release assets are served under, without a trailing slash.
    web: String,
    /// The curl program.
    curl: OsString,
    /// Refuse redirects and URLs that are not HTTPS.
    https_only: bool,
    /// The token API calls send as a bearer token.
    token: Option<String>,
    /// Running in CI, where a missing token is an error rather than an anonymous call.
    ci: bool,
    /// The wait after the first 403 or 429 without a `Retry-After`; it doubles each time.
    backoff: Duration,
}

/// The first of `names` that is set to something in the environment.
fn env_token(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.trim().is_empty()))
        .map(|v| v.trim().to_owned())
}

impl GitHub {
    /// `api.github.com` and `github.com` over HTTPS with the `curl` on `PATH`.
    #[must_use]
    pub fn public() -> Self {
        Self {
            api: "https://api.github.com".into(),
            web: "https://github.com".into(),
            curl: "curl".into(),
            https_only: true,
            token: env_token(&["GH_TOKEN", "GITHUB_TOKEN"]),
            ci: std::env::var_os("CI").is_some_and(|v| !v.is_empty() && v != "false"),
            backoff: Duration::from_secs(5),
        }
    }

    /// Another host and curl program (a local test server speaks plain HTTP).
    #[must_use]
    pub fn at(api: &str, web: &str, curl: impl Into<OsString>) -> Self {
        Self {
            api: api.trim_end_matches('/').to_owned(),
            web: web.trim_end_matches('/').to_owned(),
            curl: curl.into(),
            https_only: false,
            token: None,
            ci: false,
            backoff: Duration::from_millis(1),
        }
    }

    /// The same client sending `token` on API calls.
    #[must_use]
    pub fn with_token(mut self, token: &str) -> Self {
        self.token = Some(token.to_owned());
        self
    }

    /// The same client behaving as in CI (`ci`) or not.
    #[must_use]
    pub fn in_ci(mut self, ci: bool) -> Self {
        self.ci = ci;
        self
    }

    #[cfg(test)]
    pub(crate) fn api_root(&self) -> String {
        self.api.clone()
    }

    /// Where `git clone` fetches the repository `slug` (`owner/name`) from.
    #[must_use]
    pub fn clone_url(&self, slug: &str) -> String {
        format!("{}/{slug}.git", self.web)
    }

    fn curl(&self) -> Command {
        self.curl_with(true)
    }

    /// `fail` makes curl exit non-zero on an HTTP error status; the API calls read the status
    /// themselves.
    fn curl_with(&self, fail: bool) -> Command {
        let mut cmd = Command::new(&self.curl);
        if fail {
            cmd.arg("--fail");
        }
        cmd.args(["--silent", "--show-error", "--location"]);
        // A stalled connection must end the task, not hold the CI job until its own limit.
        cmd.args(["--connect-timeout", "30", "--max-time", "900"]);
        if self.https_only {
            cmd.args(["--proto", "=https", "--proto-redir", "=https"]);
        }
        cmd
    }

    /// The body of API `path` (`repos/<owner>/<name>/commits/<tag>`); `raw` asks for file contents
    /// as they are rather than JSON.
    ///
    /// Sends the token when there is one. A 403 or 429 is tried again up to `API_TRIES` times,
    /// waiting for `Retry-After` when GitHub sends it, else doubling from the base wait.
    ///
    /// # Errors
    ///
    /// When curl is missing (the error names the fix), there is no token in CI, the request
    /// fails or GitHub answers an error status (after the retries, with the rate-limit headers).
    pub fn api(&self, path: &str, raw: bool) -> Result<String> {
        if self.ci && self.token.is_none() {
            return Err(XtaskError::Parse {
                what: format!("the GitHub API call `{path}`"),
                reason:
                    "no GitHub token: GH_TOKEN and GITHUB_TOKEN are both unset or empty in CI, \
                    where anonymous calls share the runner's rate limit and get a 403; set \
                    `env: GH_TOKEN: ${{ github.token }}` on the workflow step that runs xtask"
                        .into(),
            });
        }
        let accept = if raw {
            "Accept: application/vnd.github.raw"
        } else {
            "Accept: application/vnd.github+json"
        };
        // curl reads the token from a config on stdin: argv (shown in errors, listed by `ps`) never holds it
        let config = self.token.as_ref().map_or_else(String::new, |t| {
            let t: String = t.chars().filter(|c| !c.is_control() && *c != '"').collect();
            format!("header = \"Authorization: Bearer {t}\"\n")
        });
        let url = format!("{}/{path}", self.api);
        let mut wait = self.backoff;
        let mut attempt = 1;
        loop {
            let mut cmd = self.curl_with(false);
            cmd.args(["--include", "--config", "-", "-H", accept, &url]);
            let out = run_with_stdin(&mut cmd, config.as_bytes())?;
            let reply = Reply::parse(&out);
            match reply.status {
                200..=299 => return Ok(reply.body.to_owned()),
                403 | 429 if attempt < API_TRIES => {
                    std::thread::sleep(reply.wait(wait));
                    wait *= 2;
                    attempt += 1;
                }
                status => {
                    return Err(XtaskError::Tool {
                        command: format!("{} GET {url}", self.curl.to_string_lossy()),
                        detail: reply.failure(status, attempt, self.token.is_some()),
                    });
                }
            }
        }
    }

    /// Release asset `name` of `repo`'s release `tag`, saved into `dir`; returns its path.
    ///
    /// # Errors
    ///
    /// When curl is missing (the error names the fix) or the download fails; a partial file is
    /// not left behind.
    pub fn release_asset(&self, repo: &str, tag: &str, name: &str, dir: &Path) -> Result<PathBuf> {
        let target = dir.join(name);
        let result = run(self.curl().arg("--output").arg(&target).arg(format!(
            "{}/{repo}/releases/download/{tag}/{name}",
            self.web
        )));
        if result.is_err() {
            // best-effort removal of the partial download; the download error is what the caller gets
            let _ = std::fs::remove_file(&target);
        }
        result.map(|_| target)
    }
}

/// What `curl --include` printed for the last response of a call: its status, headers and body.
struct Reply<'a> {
    status: u16,
    headers: Vec<(String, &'a str)>,
    body: &'a str,
}

impl<'a> Reply<'a> {
    /// Skips the header blocks of redirects and `100 Continue`, keeps the last one.
    fn parse(mut text: &'a str) -> Self {
        loop {
            let (head, rest) = text
                .split_once("\r\n\r\n")
                .map_or((text, ""), |(h, r)| (h, r));
            let mut lines = head.lines();
            let status = lines
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(0);
            let more = (status == 100 || (300..400).contains(&status)) && rest.starts_with("HTTP/");
            if more {
                text = rest;
                continue;
            }
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim()))
                .collect();
            return Self {
                status,
                headers,
                body: rest,
            };
        }
    }

    fn header(&self, name: &str) -> Option<&'a str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| *v)
    }

    /// How long to wait before the next try: GitHub's `Retry-After`, else the rate-limit reset
    /// when the budget is spent, else `backoff`; never over [`MAX_WAIT`].
    fn wait(&self, backoff: Duration) -> Duration {
        let told = self
            .header("retry-after")
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        let reset = (self.header("x-ratelimit-remaining") == Some("0"))
            .then(|| self.header("x-ratelimit-reset")?.parse::<u64>().ok())
            .flatten()
            .and_then(|at| {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?;
                Some(Duration::from_secs(at).saturating_sub(now))
            });
        told.or(reset).unwrap_or(backoff).min(MAX_WAIT)
    }

    /// The error text of a call that ended with `status`.
    fn failure(&self, status: u16, tries: u32, sent_token: bool) -> String {
        let mut text = format!("GitHub answered HTTP {status}");
        if tries > 1 {
            text = format!("{text} on each of {tries} tries");
        }
        let limits: Vec<String> = [
            "retry-after",
            "x-ratelimit-limit",
            "x-ratelimit-remaining",
            "x-ratelimit-used",
            "x-ratelimit-reset",
            "x-ratelimit-resource",
        ]
        .iter()
        .filter_map(|n| self.header(n).map(|v| format!("{n}: {v}")))
        .collect();
        if !limits.is_empty() {
            text = format!("{text} ({})", limits.join(", "));
        }
        let message = serde_json::from_str::<serde_json::Value>(self.body)
            .ok()
            .and_then(|v| v.get("message")?.as_str().map(str::to_owned));
        if let Some(message) = message {
            text = format!("{text}: {message}");
        }
        if matches!(status, 403 | 429) {
            text.push_str(if sent_token {
                "; the token was sent, so its own budget or its permissions are the limit"
            } else {
                "; no token was sent: set GH_TOKEN (CI: `${{ github.token }}`) to use the token's larger budget"
            });
        }
        text
    }
}

#[cfg(test)]
pub(crate) mod tests;
