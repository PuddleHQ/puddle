// SPDX-License-Identifier: AGPL-3.0-or-later
//! VS Code's Machine settings in the guest: how ports are forwarded and the
//! accident guard that keeps desktop VS Code from handing its git credentials to the guest.

use puddle_types::{
    GuestFile, MergeEntry, MergeFormat, MergeSpec, SessionUser, ValidationError, guest_path,
};
use serde_json::json;

/// Where the VS Code server reads Machine settings, below the session user's home.
pub const MACHINE_SETTINGS_RELATIVE: &str = ".vscode-server/data/Machine/settings.json";

/// Where the VS Code server reads Machine settings for the `root` user.
pub const MACHINE_SETTINGS_GUEST: &str = "/root/.vscode-server/data/Machine/settings.json";

/// Where the VS Code server reads Machine settings for the `agent` user.
const MACHINE_SETTINGS_AGENT: &str = "/home/agent/.vscode-server/data/Machine/settings.json";

/// The Machine settings file of `user`.
#[must_use]
pub fn machine_settings_path(user: SessionUser) -> String {
    match user {
        SessionUser::Agent => MACHINE_SETTINGS_AGENT,
        SessionUser::Root => MACHINE_SETTINGS_GUEST,
    }
    .to_owned()
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
/// # Errors
///
/// Never in practice: the keys are fixed and disjoint (the unit tests build the spec for several
/// ports), but the spec's own check returns an error, so it is passed on.
pub fn machine_settings(agent_port: u16, user: SessionUser) -> Result<GuestFile, ValidationError> {
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
    let path = match user {
        SessionUser::Agent => guest_path!(MACHINE_SETTINGS_AGENT),
        SessionUser::Root => guest_path!(MACHINE_SETTINGS_GUEST),
    };
    Ok(GuestFile::merged(path, spec?))
}

#[cfg(test)]
mod tests;
