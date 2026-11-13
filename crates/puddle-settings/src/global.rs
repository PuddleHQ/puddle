// SPDX-License-Identifier: GPL-3.0-or-later
//! The global settings document: one per user.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::document::{self, Document};
use crate::migrate::{self, Migration};
use crate::{
    AppIconChoice, CloseBehaviour, ConsentKind, Consents, DensityChoice, DiskSpaceLimits,
    DiskWarningPercent, FirstRun, FreeGb, HexColour, Loaded, PaletteChoice, ServerChoice,
    SettingsError, TermsVersion, ThemeChoice, WorkspaceLayer,
};

/// The version of the terms the enable popup for Microsoft's server shows: the licence's address.
/// A consent recorded for any other version does not cover the server ([`crate::Consent::allows`]).
/// The settings screen shows the same address (`MS_TERMS_VERSION` in `ui/src/lib/settings/model.ts`).
pub const MICROSOFT_TERMS_VERSION: &str = "https://code.visualstudio.com/license/server";

/// The current shape of [`GlobalSettings`] documents.
pub const GLOBAL_SCHEMA_VERSION: u32 = 2;

/// Puddle's settings for this user: defaults for every workspace, options that only exist
/// globally, and the consents.
///
/// ```
/// use puddle_settings::GlobalSettings;
/// use serde_json::json;
///
/// let loaded = GlobalSettings::from_document(json!({
///     "schema_version": 2,
///     "workspace_defaults": { "memory": 4096, "local_toggles": { "private": true } },
///     "consents": { "telemetry": { "state": "declined", "at": 1, "terms_version": "t1" } },
/// })).unwrap();
/// assert_eq!(loaded.settings.workspace_defaults.memory.unwrap().get(), 4096);
/// assert!(loaded.unknown_fields.is_empty());
///
/// let doc = loaded.settings.to_document();
/// assert_eq!(doc["schema_version"], 2);
/// assert_eq!(GlobalSettings::from_document(doc).unwrap().settings, loaded.settings);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GlobalSettings {
    /// The value every workspace gets unless it overrides it. Unset values fall back to Puddle's
    /// built-in defaults.
    #[serde(default, skip_serializing_if = "WorkspaceLayer::is_empty")]
    pub workspace_defaults: WorkspaceLayer,
    /// Options for Microsoft's VS Code server (global only).
    #[serde(default, skip_serializing_if = "VsCodeServer::is_empty")]
    pub vscode_server: VsCodeServer,
    /// How Puddle's own window looks and behaves (global only).
    #[serde(default, skip_serializing_if = "UiPrefs::is_empty")]
    pub ui: UiPrefs,
    /// The managed tools layer (mise); global only. Its switch removes the layer for every
    /// workspace, so no workspace value can override it.
    #[serde(default, skip_serializing_if = "Tools::is_empty")]
    pub tools: Tools,
    /// What the user agreed to or declined (per user, never per workspace).
    #[serde(default, skip_serializing_if = "Consents::is_empty")]
    pub consents: Consents,
    /// Whether the first-run flow has been through (per user).
    #[serde(default, skip_serializing_if = "FirstRun::is_empty")]
    pub first_run: FirstRun,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl GlobalSettings {
    /// Reads a stored document: checks its version, migrates it, keeps unknown fields.
    ///
    /// # Errors
    ///
    /// When the document isn't an object, has a bad or newer `schema_version`, a migration
    /// fails, or a value is invalid. See the crate docs for the rules.
    pub fn from_document(doc: Value) -> Result<Loaded<Self>, SettingsError> {
        document::load(doc)
    }

    /// The document to store: the current `schema_version`, set values only, unknown fields
    /// as they were read.
    #[must_use]
    pub fn to_document(&self) -> Value {
        document::store(self)
    }

    /// Whether the user's recorded consent covers Microsoft's server under the terms Puddle
    /// shows today ([`MICROSOFT_TERMS_VERSION`]). A decline, no answer and an agreement to other
    /// terms all say no.
    #[must_use]
    pub fn microsoft_consented(&self) -> bool {
        TermsVersion::new(MICROSOFT_TERMS_VERSION)
            .is_ok_and(|terms| self.consents.get(ConsentKind::VsCodeServer).allows(&terms))
    }

    /// The server the browser editor runs: Microsoft's only when it is chosen *and* the consent
    /// covers it ([`Self::microsoft_consented`]); anything else, a choice that outlived its consent
    /// included, is the bundled code-server. Everything that starts or allows Microsoft's server
    /// asks here, never the choice alone.
    #[must_use]
    pub fn editor_server(&self) -> ServerChoice {
        match self.vscode_server.server() {
            ServerChoice::Microsoft if self.microsoft_consented() => ServerChoice::Microsoft,
            _ => ServerChoice::CodeServer,
        }
    }
}

impl Document for GlobalSettings {
    const KIND: &'static str = "global";
    const VERSION: u32 = GLOBAL_SCHEMA_VERSION;
    const MIGRATIONS: &'static [Migration] = migrate::GLOBAL;

    fn collect_unknown(&self, out: &mut Vec<String>) {
        document::push_unknown("", &self.extra, out);
        self.workspace_defaults
            .collect_unknown("workspace_defaults.", out);
        document::push_unknown("vscode_server.", &self.vscode_server.extra, out);
        document::push_unknown("ui.", &self.ui.extra, out);
        document::push_unknown("tools.", &self.tools.extra, out);
        self.consents.collect_unknown("consents.", out);
        document::push_unknown("first_run.", &self.first_run.extra, out);
    }
}

/// Options for Microsoft's VS Code server once the user enabled it. Whether it is
/// enabled at all is [`Consents::vscode_server`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VsCodeServer {
    /// Which server browser VS Code runs; default the bundled code-server. Choosing Microsoft's
    /// needs a granted [`Consents::vscode_server`], which the API checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerChoice>,
    /// Let the server send Microsoft its telemetry: the checkbox in the enable popup, default
    /// off (unchecked passes `--disable-telemetry`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<bool>,
    /// Update the server automatically when no window is connected; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl VsCodeServer {
    /// Puddle's default for [`VsCodeServer::telemetry`].
    pub const DEFAULT_TELEMETRY: bool = false;
    /// Puddle's default for [`VsCodeServer::auto_update`].
    pub const DEFAULT_AUTO_UPDATE: bool = true;

    /// The server in use.
    #[must_use]
    pub fn server(&self) -> ServerChoice {
        self.server.unwrap_or_default()
    }

    /// Whether the server's telemetry is on.
    #[must_use]
    pub fn telemetry(&self) -> bool {
        self.telemetry.unwrap_or(Self::DEFAULT_TELEMETRY)
    }

    /// Whether the server updates itself.
    #[must_use]
    pub fn auto_update(&self) -> bool {
        self.auto_update.unwrap_or(Self::DEFAULT_AUTO_UPDATE)
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The managed tools layer. Global only: it is deliberately not part of
/// [`crate::WorkspaceLayer`], whose values a workspace may override.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Tools {
    /// The global switch of the managed mise layer: off removes every piece of it for every
    /// workspace (see [`crate::mise_mode`]); default on. Switching it off keeps what is stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mise: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl Tools {
    /// Puddle's default for [`Tools::mise`].
    pub const DEFAULT_MISE: bool = true;

    /// Whether the managed mise layer is on at all.
    #[must_use]
    pub fn mise(&self) -> bool {
        self.mise.unwrap_or(Self::DEFAULT_MISE)
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Preferences for Puddle's own window. They are stored here so they follow the user; the
/// desktop shell reads the notification and close settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UiPrefs {
    /// Light, dark or follow the system; default follow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<ThemeChoice>,
    /// Comfortable or compact spacing for the whole app; default comfortable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<DensityChoice>,
    /// The colour palette over the light or dark base; default Pond.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<PaletteChoice>,
    /// Custom palette: the accent colour; Pond's where unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_accent: Option<HexColour>,
    /// Custom palette: the page background; Pond's where unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_background: Option<HexColour>,
    /// Custom palette: the cards and panels; Pond's where unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_surface: Option<HexColour>,
    /// Custom palette: the text; Pond's where unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_text: Option<HexColour>,
    /// Show a system notification for a new request; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<bool>,
    /// Play the system sound with a notification; default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sound: Option<bool>,
    /// What the close button does; default keep running in the tray.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_behaviour: Option<CloseBehaviour>,
    /// Whether minimizing the window hides it to the tray instead of the taskbar; default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimize_to_tray: Option<bool>,
    /// The icon of the window, the tray and the taskbar; default the duck.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_icon: Option<AppIconChoice>,
    /// Raise the system notification only while Puddle's window is not focused; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_only_unfocused: Option<bool>,
    /// Show the waiting count on the tray, the taskbar and the window title; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<bool>,
    /// Notify when a workspace is killed for running out of memory; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_oom: Option<bool>,
    /// Notify when a workspace's root disk or volume reaches the warning threshold; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_disk_full: Option<bool>,
    /// How full a workspace's root disk or volume gets, in percent, before Puddle warns
    /// (the banner on the workspace and, when [`UiPrefs::notify_disk_full`] is on, the
    /// notification); default 95.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_warning_percent: Option<DiskWarningPercent>,
    /// Notify when a workspace stops or crashes without being asked to; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_unexpected_stop: Option<bool>,
    /// Ask for confirmation before a decision creates a rule for every workspace; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_global_rules: Option<bool>,
    /// The single-key shortcuts of the inbox (A, D, J, K); default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_shortcuts: Option<bool>,
    /// Start Puddle when the user signs in to the computer; default off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_at_login: Option<bool>,
    /// Notify when the free space on the drive that holds Puddle's data falls under the warning
    /// or the failure line; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_low_disk_space: Option<bool>,
    /// The free space, in GB, under which `puddle doctor` warns and the app says so; default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_warn_free_gb: Option<FreeGb>,
    /// The free space, in GB, under which `puddle doctor` fails; default 5. Never above the
    /// warning line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_fail_free_gb: Option<FreeGb>,
    /// The SSH agent Puddle asks for the key list: a named pipe on Windows (default
    /// `\\.\pipe\openssh-ssh-agent`; Pageant's OpenSSH pipe goes here), a socket path elsewhere
    /// (default the `SSH_AUTH_SOCK` of Puddle's session). A blank value counts as unset. Set in the
    /// settings file; no screen edits it yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_agent_pipe: Option<String>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl UiPrefs {
    /// Puddle's default for [`UiPrefs::notifications`].
    pub const DEFAULT_NOTIFICATIONS: bool = true;
    /// Puddle's default for [`UiPrefs::sound`].
    pub const DEFAULT_SOUND: bool = false;
    /// Puddle's default for [`UiPrefs::minimize_to_tray`].
    pub const DEFAULT_MINIMIZE_TO_TRAY: bool = false;
    /// Puddle's default for [`UiPrefs::notify_only_unfocused`].
    pub const DEFAULT_NOTIFY_ONLY_UNFOCUSED: bool = true;
    /// Puddle's default for [`UiPrefs::badge`].
    pub const DEFAULT_BADGE: bool = true;
    /// Puddle's default for [`UiPrefs::notify_oom`].
    pub const DEFAULT_NOTIFY_OOM: bool = true;
    /// Puddle's default for [`UiPrefs::notify_disk_full`].
    pub const DEFAULT_NOTIFY_DISK_FULL: bool = true;
    /// Puddle's default for [`UiPrefs::notify_unexpected_stop`].
    pub const DEFAULT_NOTIFY_UNEXPECTED_STOP: bool = true;
    /// Puddle's default for [`UiPrefs::confirm_global_rules`].
    pub const DEFAULT_CONFIRM_GLOBAL_RULES: bool = true;
    /// Puddle's default for [`UiPrefs::inbox_shortcuts`].
    pub const DEFAULT_INBOX_SHORTCUTS: bool = false;
    /// Puddle's default for [`UiPrefs::start_at_login`].
    pub const DEFAULT_START_AT_LOGIN: bool = false;
    /// Puddle's default for [`UiPrefs::notify_low_disk_space`].
    pub const DEFAULT_NOTIFY_LOW_DISK_SPACE: bool = true;

    /// The theme in use.
    #[must_use]
    pub fn theme(&self) -> ThemeChoice {
        self.theme.unwrap_or_default()
    }

    /// The density in use.
    #[must_use]
    pub fn density(&self) -> DensityChoice {
        self.density.unwrap_or_default()
    }

    /// The palette in use.
    #[must_use]
    pub fn palette(&self) -> PaletteChoice {
        self.palette.unwrap_or_default()
    }

    /// Whether a new request raises a system notification.
    #[must_use]
    pub fn notifications(&self) -> bool {
        self.notifications.unwrap_or(Self::DEFAULT_NOTIFICATIONS)
    }

    /// Whether a notification plays the system sound.
    #[must_use]
    pub fn sound(&self) -> bool {
        self.sound.unwrap_or(Self::DEFAULT_SOUND)
    }

    /// The icon of the window, the tray and the taskbar.
    #[must_use]
    pub fn app_icon(&self) -> AppIconChoice {
        self.app_icon.unwrap_or_default()
    }

    /// What closing the window does.
    #[must_use]
    pub fn close_behaviour(&self) -> CloseBehaviour {
        self.close_behaviour.unwrap_or_default()
    }

    /// Whether minimizing hides the window to the tray.
    #[must_use]
    pub fn minimize_to_tray(&self) -> bool {
        self.minimize_to_tray
            .unwrap_or(Self::DEFAULT_MINIMIZE_TO_TRAY)
    }

    /// Whether a notification waits for the window to lose focus.
    #[must_use]
    pub fn notify_only_unfocused(&self) -> bool {
        self.notify_only_unfocused
            .unwrap_or(Self::DEFAULT_NOTIFY_ONLY_UNFOCUSED)
    }

    /// Whether the waiting count shows on the tray, taskbar and title.
    #[must_use]
    pub fn badge(&self) -> bool {
        self.badge.unwrap_or(Self::DEFAULT_BADGE)
    }

    /// Whether an out-of-memory kill notifies.
    #[must_use]
    pub fn notify_oom(&self) -> bool {
        self.notify_oom.unwrap_or(Self::DEFAULT_NOTIFY_OOM)
    }

    /// Whether a disk reaching the warning threshold notifies.
    #[must_use]
    pub fn notify_disk_full(&self) -> bool {
        self.notify_disk_full
            .unwrap_or(Self::DEFAULT_NOTIFY_DISK_FULL)
    }

    /// How full a disk gets before Puddle warns.
    #[must_use]
    pub fn disk_warning_percent(&self) -> DiskWarningPercent {
        self.disk_warning_percent.unwrap_or_default()
    }

    /// Whether an unexpected stop or crash notifies.
    #[must_use]
    pub fn notify_unexpected_stop(&self) -> bool {
        self.notify_unexpected_stop
            .unwrap_or(Self::DEFAULT_NOTIFY_UNEXPECTED_STOP)
    }

    /// Whether a decision for every workspace asks first.
    #[must_use]
    pub fn confirm_global_rules(&self) -> bool {
        self.confirm_global_rules
            .unwrap_or(Self::DEFAULT_CONFIRM_GLOBAL_RULES)
    }

    /// Whether the inbox answers to single keys.
    #[must_use]
    pub fn inbox_shortcuts(&self) -> bool {
        self.inbox_shortcuts
            .unwrap_or(Self::DEFAULT_INBOX_SHORTCUTS)
    }

    /// Whether Puddle starts when the user signs in.
    #[must_use]
    pub fn start_at_login(&self) -> bool {
        self.start_at_login.unwrap_or(Self::DEFAULT_START_AT_LOGIN)
    }

    /// Whether a drive running low on free space notifies.
    #[must_use]
    pub fn notify_low_disk_space(&self) -> bool {
        self.notify_low_disk_space
            .unwrap_or(Self::DEFAULT_NOTIFY_LOW_DISK_SPACE)
    }

    /// The warning and failure lines for free space, a failure line above the warning line
    /// lowered to it.
    #[must_use]
    pub fn disk_limits(&self) -> DiskSpaceLimits {
        DiskSpaceLimits::clamped(
            self.disk_warn_free_gb.unwrap_or(FreeGb::DEFAULT_WARN),
            self.disk_fail_free_gb.unwrap_or(FreeGb::DEFAULT_FAIL),
        )
    }

    /// The pipe or socket of the SSH agent the user named, or `None` when none is (a blank value
    /// is none).
    #[must_use]
    pub fn ssh_agent_pipe(&self) -> Option<&str> {
        self.ssh_agent_pipe
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl Consents {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests;
