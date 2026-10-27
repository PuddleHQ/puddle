// SPDX-License-Identifier: GPL-3.0-or-later
//! The one routing rule: a request belongs to workspace `name` when its `Host` is exactly
//! `name.localhost:<the front's port>`. Anything else is refused, so a page that was tricked into
//! asking for a Puddle address by another name (DNS rebinding) or by the IP address reaches nothing.

use puddle_types::WorkspaceName;

/// Why a `Host` is not a workspace's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// No `Host` header, or one that is not text.
    #[error("the request names no host")]
    Missing,
    /// Not `<name>.localhost:<port>`: another name, an IP address, another port, or more labels.
    #[error("not an address of this front")]
    Foreign,
    /// `<name>` is not a workspace name (the desktop shell's own names are among the refused).
    #[error("not a workspace name")]
    NotAWorkspace,
}

/// The workspace `host` addresses, for a front listening on `port`.
///
/// # Errors
///
/// A [`Refusal`] for anything that is not `<workspace>.localhost:<port>`.
pub fn workspace_of(host: Option<&str>, port: u16) -> Result<WorkspaceName, Refusal> {
    let host = host.ok_or(Refusal::Missing)?;
    let (name, rest) = host.split_once('.').ok_or(Refusal::Foreign)?;
    let rest = rest.to_ascii_lowercase();
    let own_port = rest.strip_prefix("localhost:").ok_or(Refusal::Foreign)?;
    // Compared as text: a browser writes the port without a sign or leading zeros.
    if own_port != port.to_string() {
        return Err(Refusal::Foreign);
    }
    WorkspaceName::new(&name.to_ascii_lowercase()).map_err(|_| Refusal::NotAWorkspace)
}

/// The origin a page on `host` has: where requests for it may come from.
#[must_use]
pub fn origin_of(host: &str) -> String {
    format!("http://{}", host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_workspace_address_names_the_workspace_in_any_case() {
        for host in ["acme.localhost:4000", "ACME.Localhost:4000"] {
            assert_eq!(workspace_of(Some(host), 4000).unwrap().as_str(), "acme");
        }
    }

    #[test]
    fn every_other_address_is_refused() {
        let foreign = [
            "acme.localhost",
            "acme.localhost:4001",
            "acme.localhost:04000",
            "acme.localhost:+4000",
            "acme.localhost.:4000",
            "acme.localhost.evil.test:4000",
            "acme.sub.localhost:4000",
            "localhost:4000",
            "127.0.0.1:4000",
            "[::1]:4000",
            "evil.test:4000",
            "acme.localhost@evil.test:4000",
            "",
        ];
        for host in foreign {
            assert_eq!(
                workspace_of(Some(host), 4000),
                Err(Refusal::Foreign),
                "{host:?}"
            );
        }
        assert_eq!(workspace_of(None, 4000), Err(Refusal::Missing));
    }

    #[test]
    fn the_desktop_shells_own_names_and_bad_labels_are_not_workspaces() {
        for host in [
            "tauri.localhost:4000",
            "ipc.localhost:4000",
            "asset.localhost:4000",
            "-a.localhost:4000",
            "a_b.localhost:4000",
            ".localhost:4000",
        ] {
            assert_eq!(
                workspace_of(Some(host), 4000),
                Err(Refusal::NotAWorkspace),
                "{host:?}"
            );
        }
    }

    #[test]
    fn an_origin_is_the_host_in_lower_case() {
        assert_eq!(
            origin_of("Acme.localhost:4000"),
            "http://acme.localhost:4000"
        );
    }
}
