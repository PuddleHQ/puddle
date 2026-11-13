// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` / `NO_PROXY` variables as an [`OsProxy`]: the
//! whole OS layer on Unix, and the last fallback on Windows ([`crate::EnvFallback`]).

use std::ffi::OsString;
use std::sync::Arc;

use crate::health::ProxyProblem;
use crate::hop::ProxyAddr;
use crate::os::{
    ChangeCallback, Origin, OsProxy, PacError, PacQuery, ProblemCallback, ProxyConfig,
    SettingsError, WatchGuard,
};
use crate::parse::{BypassList, ProxyRules};

/// Proxy settings read from environment variables. Credentials in the URLs are dropped (see
/// [`ProxyAddr`]). The variables are read once, when this is built; a change of the process
/// environment is not noticed (Unix has no notification for it either).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvOs {
    rules: ProxyRules,
    bypass: BypassList,
    problems: Vec<ProxyProblem>,
}

impl EnvOs {
    /// Reads the current process environment.
    #[must_use]
    pub fn from_process() -> Self {
        Self::from_vars(std::env::vars_os())
    }

    /// Reads `vars`. Upper- and lower-case names both count; the lower-case one wins (curl's rule).
    /// A value Puddle cannot use (`socks5://`, `https://`, no host) is ignored: it is listed in
    /// [`ProxyConfig::problems`], which discovery logs and the network-health report shows (the
    /// connections it was meant for go direct).
    #[must_use]
    pub fn from_vars(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        let vars: Vec<(String, String)> = vars
            .into_iter()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect();
        let get = |name: &str| {
            let lower = name.to_ascii_lowercase();
            let pick = |key: &str| {
                vars.iter()
                    .find(|(k, v)| k == key && !v.trim().is_empty())
                    .map(|(_, v)| v.as_str())
            };
            pick(&lower).or_else(|| pick(name))
        };
        let mut problems = Vec::new();
        let mut proxy = |name: &str| -> Option<ProxyAddr> {
            let value = get(name)?;
            match ProxyAddr::parse(value) {
                Ok(addr) => Some(addr),
                Err(err) => {
                    let shown = crate::redact::entry_text(value);
                    problems.push(ProxyProblem::unusable(format!(
                        "the {name} variable (\"{shown}\"): {err}"
                    )));
                    None
                }
            }
        };
        let rules = ProxyRules::new(
            proxy("HTTP_PROXY"),
            proxy("HTTPS_PROXY"),
            proxy("ALL_PROXY"),
        );
        Self {
            rules,
            bypass: get("NO_PROXY")
                .map(BypassList::parse_no_proxy)
                .unwrap_or_default(),
            problems,
        }
    }

    pub(crate) fn config(&self) -> ProxyConfig {
        ProxyConfig {
            rules: self.rules.clone(),
            bypass: self.bypass.clone(),
            origin: Origin::Environment,
            problems: self.problems.clone(),
            ..ProxyConfig::default()
        }
    }
}

impl OsProxy for EnvOs {
    fn config(&self) -> Result<ProxyConfig, SettingsError> {
        Ok(EnvOs::config(self))
    }

    fn resolve_pac(&self, _query: &PacQuery) -> Result<Vec<crate::hop::Hop>, PacError> {
        Err(PacError::Unavailable(
            "PAC evaluation is not available on this platform yet".into(),
        ))
    }

    fn watch(
        &self,
        _on_change: ChangeCallback,
        _on_problem: ProblemCallback,
    ) -> Option<Box<dyn WatchGuard>> {
        // Nothing notifies a process of a changed environment; not a failure to report.
        None
    }
}

/// An [`OsProxy`] that asks `primary`, and when it names no static proxy, adds the environment's.
/// PAC and change notification are the primary's.
#[derive(Debug)]
pub struct EnvFallback {
    primary: Arc<dyn OsProxy>,
    env: EnvOs,
}

impl EnvFallback {
    /// `primary` first, `env` when `primary` has no static proxy.
    #[must_use]
    pub fn new(primary: Arc<dyn OsProxy>, env: EnvOs) -> Self {
        Self { primary, env }
    }
}

impl OsProxy for EnvFallback {
    fn config(&self) -> Result<ProxyConfig, SettingsError> {
        let mut config = match self.primary.config() {
            Ok(config) => config,
            Err(err) => {
                tracing::warn!(error = %err, "system proxy settings unreadable; using the environment");
                ProxyConfig {
                    read_error: Some(err.0),
                    ..ProxyConfig::default()
                }
            }
        };
        // PAC settings stay the primary's (and are tried first); the environment backs them up
        // when the primary names no static proxy, so a PAC outage still has somewhere to go.
        if config.rules.is_empty() {
            let env = self.env.config();
            config.rules = env.rules;
            config.bypass = env.bypass;
            config.origin = Origin::Environment;
            config.problems.extend(env.problems);
        }
        Ok(config)
    }

    fn resolve_pac(&self, query: &PacQuery) -> Result<Vec<crate::hop::Hop>, PacError> {
        self.primary.resolve_pac(query)
    }

    fn watch(
        &self,
        on_change: ChangeCallback,
        on_problem: ProblemCallback,
    ) -> Option<Box<dyn WatchGuard>> {
        self.primary.watch(on_change, on_problem)
    }
}

#[cfg(test)]
mod tests;
