// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain HTTPS reads of public GitHub data: release assets and API answers, fetched with `curl`.
//!
//! No `gh` and no sign-in: the forks and upstream are public, so a host that has only `git` and
//! `curl` (a fresh server, a CI runner) can build the runtime folder. Callers verify what they
//! download against a checksum list; this module only moves bytes.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Result;
use crate::tools::run;

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
        }
    }

    fn curl(&self) -> Command {
        let mut cmd = Command::new(&self.curl);
        cmd.args(["--fail", "--silent", "--show-error", "--location"]);
        if self.https_only {
            cmd.args(["--proto", "=https", "--proto-redir", "=https"]);
        }
        cmd
    }

    /// The body of API `path` (`repos/<owner>/<name>/commits/<tag>`); `raw` asks for file contents
    /// as they are rather than JSON.
    ///
    /// # Errors
    ///
    /// When curl is missing (the error names the fix), the request fails or GitHub answers an
    /// error status.
    pub fn api(&self, path: &str, raw: bool) -> Result<String> {
        let accept = if raw {
            "Accept: application/vnd.github.raw"
        } else {
            "Accept: application/vnd.github+json"
        };
        run(self
            .curl()
            .args(["-H", accept])
            .arg(format!("{}/{path}", self.api)))
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

#[cfg(test)]
pub(crate) mod tests;
