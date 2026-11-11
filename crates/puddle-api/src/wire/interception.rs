// SPDX-License-Identifier: GPL-3.0-or-later
//! Decrypting every host on the wire: a workspace's switch and the names that are never decrypted.

use puddle_types::{InterceptionMode, WorkspaceId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A workspace's own choice of interception mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    /// Follow the global setting.
    Inherit,
    /// Decrypt every host.
    All,
    /// Decrypt every host except the curated list.
    AllExceptCurated,
    /// Decrypt only the hosts Puddle has a reason to read.
    KnownOnly,
}

impl WorkspaceMode {
    /// The override this choice stands for (`None` follows the global setting).
    #[must_use]
    pub fn own(self) -> Option<InterceptionMode> {
        match self {
            Self::Inherit => None,
            Self::All => Some(InterceptionMode::All),
            Self::AllExceptCurated => Some(InterceptionMode::AllExceptCurated),
            Self::KnownOnly => Some(InterceptionMode::KnownOnly),
        }
    }

    /// The choice a stored override stands for.
    #[must_use]
    pub fn of(own: Option<InterceptionMode>) -> Self {
        match own {
            None => Self::Inherit,
            Some(InterceptionMode::All) => Self::All,
            Some(InterceptionMode::AllExceptCurated) => Self::AllExceptCurated,
            Some(InterceptionMode::KnownOnly) => Self::KnownOnly,
        }
    }
}

/// How a workspace decrypts hosts beyond the ones it has a credential, secret, login capture or
/// rule on method and path for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InterceptionView {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// The workspace's own choice; `inherit` follows the global setting.
    pub mode: WorkspaceMode,
    /// The mode in force: the workspace's own, or the global one.
    pub effective_mode: InterceptionMode,
    /// The names this workspace never decrypts while it decrypts unlisted hosts (`example.com`,
    /// `*.example.com`), sorted.
    pub never_decrypt: Vec<String>,
    /// The names no workspace decrypts, sorted. Changed through the global route.
    pub global_never_decrypt: Vec<String>,
}

/// A change to a workspace's interception. A field that is absent stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct InterceptionRequest {
    /// Sets the workspace's own mode. It applies to the running workspace at once.
    #[serde(default)]
    pub mode: Option<WorkspaceMode>,
    /// Replaces the workspace's never-decrypt names. Each is a host name or `*.` followed by a
    /// domain with at least two labels; no IP addresses. At most 500.
    #[serde(default)]
    pub never_decrypt: Option<Vec<String>>,
}

/// The global interception setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InterceptionSettingsView {
    /// The mode every workspace without its own follows. An install that has not chosen is on
    /// `known_only`.
    pub mode: InterceptionMode,
    /// Whether the user has chosen a mode. While `false`, the app offers the choice once.
    pub chosen: bool,
    /// Whether a host whose tool refuses Puddle's certificate is passed through for a while and
    /// put on the learned list. Meaningful in the modes that decrypt unlisted hosts. On unless
    /// turned off.
    pub learn_from_rejections: bool,
    /// Whether the user has seen the notice that offers the choice (existing installs).
    pub notice_seen: bool,
    /// The curated list passed through in `all_except_curated`: built in, "System managed",
    /// sorted.
    pub curated: Vec<String>,
}

/// A change to the global interception setting. A field that is absent stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct InterceptionSettingsRequest {
    /// Chooses the mode.
    #[serde(default)]
    pub mode: Option<InterceptionMode>,
    /// Turns learning from rejections on or off.
    #[serde(default)]
    pub learn_from_rejections: Option<bool>,
    /// Records that the user has seen the notice.
    #[serde(default)]
    pub notice_seen: Option<bool>,
}

/// The names no workspace decrypts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GlobalNeverDecrypt {
    /// The names, as for a workspace; sorted. On saving, replaces the whole list. At most 500.
    pub never_decrypt: Vec<String>,
}

/// A host Puddle stopped decrypting in a workspace because a tool there refused Puddle's
/// certificate for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LearnedHost {
    /// The host name, lower case.
    pub host: String,
    /// Seconds until Puddle decrypts the host again by itself.
    pub expires_in_secs: u64,
}

/// The hosts a running workspace passes through because its tools refused Puddle's certificate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LearnedHosts {
    /// Soonest to expire first. Empty for a workspace that is not running: the list is kept in
    /// memory and a start begins with none.
    pub hosts: Vec<LearnedHost>,
}

/// How long and how many hosts Puddle passes through after a tool refused its certificate. The
/// same for every workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LearnedLimits {
    /// Seconds a host is passed through, 60 to 2 592 000 (30 days). Built in: 43 200 (12 hours).
    pub ttl_secs: u32,
    /// Hosts remembered per workspace, 1 to 10 000; the oldest makes room. Built in: 1024.
    pub cap: u32,
}
