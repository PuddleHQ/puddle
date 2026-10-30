// SPDX-License-Identifier: GPL-3.0-or-later
//! The one routing rule: a request belongs to workspace `name` when its `Host` is exactly
//! `name.localhost:<the front's port>`, and to one port of that workspace's guest when it is
//! `<port>.name.localhost:<the front's port>` (Puddle's own forward of that port, where the editor's
//! `/proxy/<port>/` links go). Anything else is refused, so a page that was tricked into asking for
//! a Puddle address by another name (DNS rebinding) or by the IP address reaches nothing.

use puddle_types::WorkspaceName;

/// Why a `Host` is not a workspace's address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// No `Host` header, or one that is not text.
    #[error("the request names no host")]
    Missing,
    /// Not `[<port>.]<name>.localhost:<port>`: another name, an IP address, another port, or more
    /// labels.
    #[error("not an address of this front")]
    Foreign,
    /// `<name>` is not a workspace name (the desktop shell's own names are among the refused).
    #[error("not a workspace name")]
    NotAWorkspace,
}

/// What a `Host` addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// The workspace.
    pub workspace: WorkspaceName,
    /// The guest port Puddle forwards, or `None` for the workspace's editor.
    pub guest_port: Option<u16>,
}

/// A guest port as a host label writes it: digits only, no leading zero, 1 to 65535.
fn guest_port(label: &str) -> Option<u16> {
    let plain = label.bytes().all(|b| b.is_ascii_digit()) && !label.starts_with('0');
    plain.then(|| label.parse().ok()).flatten()
}

/// What `host` addresses, for a front listening on `port`.
///
/// # Errors
///
/// A [`Refusal`] for anything that is not `[<guest port>.]<workspace>.localhost:<port>`.
pub fn route_of(host: Option<&str>, port: u16) -> Result<Route, Refusal> {
    let host = host.ok_or(Refusal::Missing)?.to_ascii_lowercase();
    // Compared as text: a browser writes the port without a sign or leading zeros.
    let names = host
        .strip_suffix(&format!(".localhost:{port}"))
        .ok_or(Refusal::Foreign)?;
    let (guest_port, name) = match names.split_once('.') {
        Some((label, name)) => (Some(guest_port(label).ok_or(Refusal::Foreign)?), name),
        None => (None, names),
    };
    if name.contains('.') {
        return Err(Refusal::Foreign);
    }
    let workspace = WorkspaceName::new(name).map_err(|_| Refusal::NotAWorkspace)?;
    Ok(Route {
        workspace,
        guest_port,
    })
}

/// The origin a page on `host` has: where requests for it may come from.
#[must_use]
pub fn origin_of(host: &str) -> String {
    format!("http://{}", host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(host: &str) -> Result<(String, Option<u16>), Refusal> {
        route_of(Some(host), 4000).map(|r| (r.workspace.to_string(), r.guest_port))
    }

    #[test]
    fn a_workspace_address_names_the_workspace_in_any_case() {
        for host in ["acme.localhost:4000", "ACME.Localhost:4000"] {
            assert_eq!(route(host), Ok(("acme".to_owned(), None)));
        }
    }

    #[test]
    fn a_port_label_in_front_names_the_forwarded_guest_port() {
        for (host, port) in [
            ("5173.acme.localhost:4000", 5173),
            ("1.ACME.localhost:4000", 1),
            ("65535.acme.localhost:4000", 65535),
        ] {
            assert_eq!(route(host), Ok(("acme".to_owned(), Some(port))), "{host}");
        }
        // A workspace may be named by digits: that is its editor, not a port.
        assert_eq!(route("5173.localhost:4000"), Ok(("5173".to_owned(), None)));
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
            "0.acme.localhost:4000",
            "05173.acme.localhost:4000",
            "65536.acme.localhost:4000",
            "+80.acme.localhost:4000",
            "80x.acme.localhost:4000",
            "80.acme.sub.localhost:4000",
            "80.81.acme.localhost:4000",
            "80.acme.localhost:4001",
            "80.acme.localhost",
        ];
        for host in foreign {
            assert_eq!(route(host), Err(Refusal::Foreign), "{host:?}");
        }
        assert_eq!(route_of(None, 4000), Err(Refusal::Missing));
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
            "80.tauri.localhost:4000",
            "80..localhost:4000",
        ] {
            assert_eq!(route(host), Err(Refusal::NotAWorkspace), "{host:?}");
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
