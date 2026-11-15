// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one routing rule: a request belongs to workspace `name` when its `Host` is exactly
//! `name.localhost:<the front's port>`, and to one port of that workspace's guest when it is
//! `<port>.name.localhost:<the front's port>` (Puddle's own forward of that port, where the editor's
//! `/proxy/<port>/` links go). Anything else is refused, so a page that was tricked into asking for
//! a Puddle address by another name (DNS rebinding) or by the IP address reaches nothing.

use puddle_types::WorkspaceId;

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
    pub workspace: WorkspaceId,
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
    let workspace = WorkspaceId::new(name).map_err(|_| Refusal::NotAWorkspace)?;
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
mod tests;
