// SPDX-License-Identifier: AGPL-3.0-or-later
//! The settings every workspace has, each optional. The same type holds the global defaults
//! ([`crate::GlobalSettings::workspace_defaults`]) and one workspace's overrides
//! ([`crate::WorkspaceSettings::overrides`]); `None` means "not set here, ask the next level".

use std::collections::BTreeMap;

use puddle_types::{CpuCount, LocalCategory, MemoryMib};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ClipboardRead, ReconnectionGrace};

/// The per-workspace settings, each optional. A setting added here gets a global default and a
/// per-workspace override at once, and [`crate::resolve`] must learn it (its exhaustive
/// destructuring fails to compile until it does).
///
/// ```
/// use puddle_settings::WorkspaceLayer;
/// use puddle_types::{LocalCategory, MemoryMib};
/// let mut layer = WorkspaceLayer::default();
/// layer.memory = Some(MemoryMib::new(2048).unwrap());
/// layer.local_toggles.private = Some(true);
/// assert!(!layer.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceLayer {
    /// Guest memory, msb's `--memory`; applies at the workspace's next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryMib>,
    /// Guest vCPUs, msb's `--cpus`; applies at the workspace's next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<CpuCount>,
    /// Local-destination toggles, one per address category.
    #[serde(default, skip_serializing_if = "LocalToggles::is_empty")]
    pub local_toggles: LocalToggles,
    /// Whether a wildcard or suffix allow may match a name that resolves to a local address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wildcards_reach_local: Option<bool>,
    /// Browser VS Code's reconnection grace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconnection_grace: Option<ReconnectionGrace>,
    /// Zoom hotkeys in workspace windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom_hotkeys: Option<bool>,
    /// Programmatic clipboard reads in workspace windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_read: Option<ClipboardRead>,
    /// Whether Puddle opens an SSH way into the workspace for the user's own tools (desktop
    /// VS Code, a terminal `ssh`). Off means no SSH endpoint and no ssh config entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_ssh: Option<bool>,
    /// Whether a login made inside the workspace (Claude Code, GitHub's `gh` and Copilot CLI) is
    /// captured: Puddle keeps the real token on the host and the workspace holds a stand-in.
    /// Applies at the workspace's next start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_logins: Option<bool>,
    /// Whether Puddle warns about a request that carries a credential it did not add (a token
    /// the user put in the workspace). Only the warning is switched: the request goes out either
    /// way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warn_foreign_credentials: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl WorkspaceLayer {
    /// Whether no setting is set at this level (unknown fields count as set, so they are kept).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn collect_unknown(&self, prefix: &str, out: &mut Vec<String>) {
        crate::document::push_unknown(prefix, &self.extra, out);
        self.local_toggles
            .collect_unknown(&format!("{prefix}local_toggles."), out);
    }
}

/// One toggle per local-destination category. A toggle only *permits* its category: the
/// allowlist still decides each destination. Puddle's default for each is off.
///
/// One field per [`LocalCategory`], named by its [`LocalCategory::key`] (a test checks this), so
/// the stored names and the classifier's categories can't drift apart. Read a toggle by category
/// with [`Self::get`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LocalToggles {
    /// Host loopback (`127.0.0.0/8`, `::1`, `0.0.0.0`, `::`, `localhost`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loopback: Option<bool>,
    /// Private networks (RFC 1918, CGNAT, unique-local IPv6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    /// Link-local addresses (`169.254.0.0/16` except metadata, `fe80::/10`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_local: Option<bool>,
    /// Cloud metadata endpoints (`169.254.169.254` and their names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<bool>,
    /// Other special-purpose ranges (multicast, broadcast, documentation, reserved, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub special: Option<bool>,
    /// The company network (PAC-direct and DNS-suffix names, private-access and VPN ranges).
    /// Off by default; on only makes those destinations approvable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_network: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl LocalToggles {
    /// Whether no toggle is set at this level (unknown fields count as set, so they are kept).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The toggle for `category` at this level, `None` if unset.
    ///
    /// ```
    /// use puddle_settings::LocalToggles;
    /// use puddle_types::LocalCategory;
    /// let mut t = LocalToggles::default();
    /// t.link_local = Some(true);
    /// assert_eq!(t.get(LocalCategory::LinkLocal), Some(true));
    /// assert_eq!(t.get(LocalCategory::Private), None);
    /// ```
    #[must_use]
    pub fn get(&self, category: LocalCategory) -> Option<bool> {
        match category {
            LocalCategory::Loopback => self.loopback,
            LocalCategory::Private => self.private,
            LocalCategory::LinkLocal => self.link_local,
            LocalCategory::Metadata => self.metadata,
            LocalCategory::Special => self.special,
            LocalCategory::CompanyNetwork => self.company_network,
            // A category this version has no field for is unset, so it resolves to Puddle's
            // default (off). `every_category_has_its_field` fails until a field is added.
            _ => None,
        }
    }

    fn collect_unknown(&self, prefix: &str, out: &mut Vec<String>) {
        crate::document::push_unknown(prefix, &self.extra, out);
    }
}

#[cfg(test)]
mod tests;
