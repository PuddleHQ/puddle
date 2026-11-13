// SPDX-License-Identifier: AGPL-3.0-or-later
//! VS Code's Machine settings in the guest: how ports are forwarded and the
//! accident guard that keeps desktop VS Code from handing its git credentials to the guest.

use puddle_types::{GuestFile, MergeEntry, MergeFormat, MergeSpec, SessionUser};
use serde_json::json;

use crate::assets::guest_path;

/// Where the VS Code server reads Machine settings, below the session user's home.
pub const MACHINE_SETTINGS_RELATIVE: &str = ".vscode-server/data/Machine/settings.json";

/// Where the VS Code server reads Machine settings for the `root` user.
pub const MACHINE_SETTINGS_GUEST: &str = "/root/.vscode-server/data/Machine/settings.json";

/// The Machine settings file of `user`.
#[must_use]
pub fn machine_settings_path(user: SessionUser) -> String {
    format!("{}/{MACHINE_SETTINGS_RELATIVE}", user.home())
}

/// The Machine settings file, as a JSONC merge: Puddle owns only these keys, so the
/// user's own Machine settings, comments and trailing commas survive every boot.
///
/// - `remote.autoForwardPortsSource: process` and `remote.autoForwardPortsFallback: 0`, so ports
///   are found from listening processes and VS Code never falls back to output scanning;
/// - the agent's proxy port is never auto-forwarded (`remote.portsAttributes.<port>`), other ports
///   notify (`remote.otherPortsAttributes.onAutoForward`); the user's own port entries stay;
/// - `github.gitAuthentication: false` and `git.terminalAuthentication: false` (an
///   accident guard only, desktop VS Code attach stays a trusted mode). Puddle sets them again
///   at every boot, so a user's own value for these two does not last.
///
/// # Panics
///
/// Never in practice: the keys are fixed and disjoint (the unit tests build the spec for several
/// ports).
#[must_use]
pub fn machine_settings(agent_port: u16, user: SessionUser) -> GuestFile {
    let entry = |key: &[&str], value: serde_json::Value| MergeEntry::json(key, &value);
    let port = agent_port.to_string();
    let spec = MergeSpec::new(
        MergeFormat::Jsonc,
        vec![
            entry(&["remote.autoForwardPortsSource"], json!("process")),
            entry(&["remote.autoForwardPortsFallback"], json!(0)),
            entry(
                &["remote.otherPortsAttributes", "onAutoForward"],
                json!("notify"),
            ),
            entry(
                &["remote.portsAttributes", &port],
                json!({ "onAutoForward": "ignore", "label": "Puddle proxy" }),
            ),
            entry(&["github.gitAuthentication"], json!(false)),
            entry(&["git.terminalAuthentication"], json!(false)),
        ],
    );
    #[expect(
        clippy::expect_used,
        reason = "the keys are fixed and disjoint, the values are JSON: the unit tests build it for several ports"
    )]
    let spec = spec.expect("the Machine settings spec is valid");
    GuestFile::merged(guest_path(&machine_settings_path(user)), spec)
}

#[cfg(test)]
mod tests;
