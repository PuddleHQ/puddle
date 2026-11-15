// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the main window may go.
//!
//! The main window shows Puddle's own page from the API origin and nothing else. A link or
//! redirect that leaves that origin is cancelled in the window; a web address is then handed to
//! the default browser, called from Rust (the page never gets to call an opener). Anything that
//! is not a plain web address is dropped.

use url::Url;

/// Host names the webview itself answers on Windows (`http://<name>.localhost`): Tauri's
/// protocol handler serves them on any port, so they are never an address to open.
const WEBVIEW_HOSTS: [&str; 3] = ["tauri.localhost", "ipc.localhost", "asset.localhost"];

/// What to do with a navigation of the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Navigation {
    /// Let the window go there.
    Stay,
    /// Cancel it and open the address in the default browser.
    External,
    /// Cancel it and do nothing else.
    Refuse,
}

/// The origins the main window may stay on.
#[derive(Debug, Clone)]
pub struct OriginPolicy {
    api: Url,
    dev: Option<Url>,
}

impl OriginPolicy {
    /// A policy for the API's own origin, plus a development server's origin when `dev` is set.
    #[must_use]
    pub fn new(api: &Url, dev: Option<&Url>) -> Self {
        Self {
            api: api.clone(),
            dev: dev.cloned(),
        }
    }

    /// Classifies a navigation of the main window.
    #[must_use]
    pub fn classify(&self, target: &Url) -> Navigation {
        let origin = target.origin();
        if origin == self.api.origin() || self.dev.as_ref().is_some_and(|d| origin == d.origin()) {
            return Navigation::Stay;
        }
        if target.as_str() == "about:blank" {
            return Navigation::Stay;
        }
        let web = matches!(target.scheme(), "http" | "https");
        let webview_name = target
            .host_str()
            .is_some_and(|h| WEBVIEW_HOSTS.contains(&h.to_ascii_lowercase().as_str()));
        if web && !webview_name {
            Navigation::External
        } else {
            Navigation::Refuse
        }
    }
}

#[cfg(test)]
mod tests;
