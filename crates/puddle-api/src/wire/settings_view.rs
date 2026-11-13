// SPDX-License-Identifier: AGPL-3.0-or-later
//! Settings on the wire (puddle-settings).

use puddle_settings as settings;
use puddle_types::{CpuCount, MemoryMib, WorkspaceId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;

use super::AppIconChoice;

// ---------------------------------------------------------------------------------------------
// Settings (puddle-settings)

/// The settings every workspace has. On a global document they are the defaults for every
/// workspace; on a workspace they override those. `null` means "not set here": the next level (the
/// global value, then Puddle's default) applies.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsLayer {
    /// Guest memory in MiB (256 to 1 048 576); applies at the next start.
    #[serde(default)]
    #[schema(required = true, minimum = 256, maximum = 1_048_576)]
    pub memory: Option<u32>,
    /// Guest vCPUs (1 to 32); applies at the next start.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 32)]
    pub cpus: Option<u8>,
    /// Which local destination categories may be approved (all off by default).
    #[serde(default)]
    #[schema(required = true)]
    pub local_toggles: LocalToggles,
    /// Whether a suffix rule may reach local addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub wildcards_reach_local: Option<bool>,
    /// Seconds a browser VS Code session survives a disconnect (30 to 86 400).
    #[serde(default)]
    #[schema(required = true, minimum = 30, maximum = 86_400)]
    pub reconnection_grace: Option<u32>,
    /// Whether the browser window's zoom hotkeys work.
    #[serde(default)]
    #[schema(required = true)]
    pub zoom_hotkeys: Option<bool>,
    /// What a programmatic clipboard read does.
    #[serde(default)]
    #[schema(required = true)]
    pub clipboard_read: Option<ClipboardRead>,
    /// Whether Puddle opens an SSH way into the workspace for the user's own tools (desktop
    /// VS Code, a terminal `ssh`). Off by default; while off there is no SSH endpoint and no ssh
    /// config entry. Turning it on marks the workspace trusted.
    #[serde(default)]
    #[schema(required = true)]
    pub direct_ssh: Option<bool>,
    /// Whether a login made inside the workspace (Claude Code, `gh`, Copilot CLI) is captured:
    /// Puddle keeps the real token on the host and the workspace holds a stand-in that works
    /// only through Puddle. On by default; applies at the workspace's next start.
    #[serde(default)]
    #[schema(required = true)]
    pub capture_logins: Option<bool>,
    /// Whether Puddle warns when a request carries a credential it did not add (a token the
    /// workspace holds itself). The request goes out either way; this only switches the notice
    /// and the flag in the activity log. On by default.
    #[serde(default)]
    #[schema(required = true)]
    pub warn_foreign_credentials: Option<bool>,
}

impl From<&settings::WorkspaceLayer> for SettingsLayer {
    fn from(layer: &settings::WorkspaceLayer) -> Self {
        Self {
            memory: layer.memory.map(MemoryMib::get),
            cpus: layer.cpus.map(CpuCount::get),
            local_toggles: (&layer.local_toggles).into(),
            wildcards_reach_local: layer.wildcards_reach_local,
            reconnection_grace: layer
                .reconnection_grace
                .map(settings::ReconnectionGrace::secs),
            zoom_hotkeys: layer.zoom_hotkeys,
            clipboard_read: layer.clipboard_read.map(Into::into),
            direct_ssh: layer.direct_ssh,
            capture_logins: layer.capture_logins,
            warn_foreign_credentials: layer.warn_foreign_credentials,
        }
    }
}

impl SettingsLayer {
    /// Writes these values into `layer`, leaving its unknown fields alone.
    pub(crate) fn apply_to(self, layer: &mut settings::WorkspaceLayer) -> Result<(), ApiError> {
        let invalid = |err: puddle_types::ValidationError| ApiError::invalid(err);
        let Self {
            memory,
            cpus,
            local_toggles,
            wildcards_reach_local,
            reconnection_grace,
            zoom_hotkeys,
            clipboard_read,
            direct_ssh,
            capture_logins,
            warn_foreign_credentials,
        } = self;
        layer.memory = memory.map(MemoryMib::new).transpose().map_err(invalid)?;
        layer.cpus = cpus
            .map(|c| CpuCount::new(u32::from(c)))
            .transpose()
            .map_err(invalid)?;
        local_toggles.apply_to(&mut layer.local_toggles);
        layer.wildcards_reach_local = wildcards_reach_local;
        layer.reconnection_grace = reconnection_grace
            .map(settings::ReconnectionGrace::new)
            .transpose()
            .map_err(invalid)?;
        layer.zoom_hotkeys = zoom_hotkeys;
        layer.clipboard_read = clipboard_read.map(Into::into);
        layer.direct_ssh = direct_ssh;
        layer.capture_logins = capture_logins;
        layer.warn_foreign_credentials = warn_foreign_credentials;
        Ok(())
    }
}

/// Local destination categories a workspace may approve. `null` inherits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalToggles {
    /// The host's loopback addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub loopback: Option<bool>,
    /// Private networks (RFC 1918, ULA).
    #[serde(default)]
    #[schema(required = true)]
    pub private: Option<bool>,
    /// Link-local addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub link_local: Option<bool>,
    /// Cloud metadata endpoints.
    #[serde(default)]
    #[schema(required = true)]
    pub metadata: Option<bool>,
    /// Other special-purpose ranges.
    #[serde(default)]
    #[schema(required = true)]
    pub special: Option<bool>,
    /// The company network. Off by default; on only makes those destinations approvable.
    #[serde(default)]
    #[schema(required = true)]
    pub company_network: Option<bool>,
}

impl From<&settings::LocalToggles> for LocalToggles {
    fn from(t: &settings::LocalToggles) -> Self {
        Self {
            loopback: t.loopback,
            private: t.private,
            link_local: t.link_local,
            metadata: t.metadata,
            special: t.special,
            company_network: t.company_network,
        }
    }
}

impl LocalToggles {
    pub(super) fn apply_to(self, toggles: &mut settings::LocalToggles) {
        let Self {
            loopback,
            private,
            link_local,
            metadata,
            special,
            company_network,
        } = self;
        toggles.loopback = loopback;
        toggles.private = private;
        toggles.link_local = link_local;
        toggles.metadata = metadata;
        toggles.special = special;
        toggles.company_network = company_network;
    }
}

/// What a programmatic clipboard read in the browser window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardRead {
    /// Ask each time.
    Ask,
    /// Always allow.
    Allow,
    /// Always refuse.
    Deny,
}

impl From<settings::ClipboardRead> for ClipboardRead {
    fn from(c: settings::ClipboardRead) -> Self {
        match c {
            settings::ClipboardRead::Ask => Self::Ask,
            settings::ClipboardRead::Allow => Self::Allow,
            settings::ClipboardRead::Deny => Self::Deny,
        }
    }
}

impl From<ClipboardRead> for settings::ClipboardRead {
    fn from(c: ClipboardRead) -> Self {
        match c {
            ClipboardRead::Ask => Self::Ask,
            ClipboardRead::Allow => Self::Allow,
            ClipboardRead::Deny => Self::Deny,
        }
    }
}

/// Options for the VS Code server in the guest. `null` means Puddle's default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VsCodeServer {
    /// Which server browser VS Code runs (default `code_server`). `microsoft` is refused
    /// until the user agreed to Microsoft's terms (`PUT /api/consents/vscode_server`).
    #[serde(default)]
    #[schema(required = true)]
    pub server: Option<ServerChoice>,
    /// Whether the server may send Microsoft telemetry (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub telemetry: Option<bool>,
    /// Whether Puddle updates the server (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub auto_update: Option<bool>,
}

/// The global tool switches. Global only: no workspace value can override them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalTools {
    /// Whether Puddle manages tools with mise in workspaces (default on). Off removes the whole
    /// managed layer for every workspace and keeps every stored value.
    #[serde(default)]
    #[schema(required = true)]
    pub mise: Option<bool>,
}

/// One workspace's tool switches. `null` means Puddle's default (on). While the global switch
/// (`GlobalTools::mise`) is off, they are kept but have no effect.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTools {
    /// Manage this workspace's tools with mise (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub mise: Option<bool>,
    /// Let mise run the repository's own code in this workspace, that is trust its `mise.toml`
    /// (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub repo_code: Option<bool>,
}

impl From<&settings::WorkspaceTools> for WorkspaceTools {
    fn from(t: &settings::WorkspaceTools) -> Self {
        Self {
            mise: t.mise,
            repo_code: t.repo_code,
        }
    }
}

/// Which VS Code server browser VS Code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerChoice {
    /// The bundled code-server.
    CodeServer,
    /// Microsoft's VS Code server, downloaded after the user's consent.
    Microsoft,
}

impl From<settings::ServerChoice> for ServerChoice {
    fn from(c: settings::ServerChoice) -> Self {
        match c {
            settings::ServerChoice::CodeServer => Self::CodeServer,
            settings::ServerChoice::Microsoft => Self::Microsoft,
        }
    }
}

impl From<ServerChoice> for settings::ServerChoice {
    fn from(c: ServerChoice) -> Self {
        match c {
            ServerChoice::CodeServer => Self::CodeServer,
            ServerChoice::Microsoft => Self::Microsoft,
        }
    }
}

/// The colour theme of Puddle's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the operating system.
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl From<settings::ThemeChoice> for ThemeChoice {
    fn from(c: settings::ThemeChoice) -> Self {
        match c {
            settings::ThemeChoice::System => Self::System,
            settings::ThemeChoice::Light => Self::Light,
            settings::ThemeChoice::Dark => Self::Dark,
        }
    }
}

impl From<ThemeChoice> for settings::ThemeChoice {
    fn from(c: ThemeChoice) -> Self {
        match c {
            ThemeChoice::System => Self::System,
            ThemeChoice::Light => Self::Light,
            ThemeChoice::Dark => Self::Dark,
        }
    }
}

/// How much room the window leaves around its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DensityChoice {
    /// The roomier layout.
    Comfortable,
    /// Tighter spacing, so more fits on screen.
    Compact,
}

impl From<settings::DensityChoice> for DensityChoice {
    fn from(c: settings::DensityChoice) -> Self {
        match c {
            settings::DensityChoice::Comfortable => Self::Comfortable,
            settings::DensityChoice::Compact => Self::Compact,
        }
    }
}

impl From<DensityChoice> for settings::DensityChoice {
    fn from(c: DensityChoice) -> Self {
        match c {
            DensityChoice::Comfortable => Self::Comfortable,
            DensityChoice::Compact => Self::Compact,
        }
    }
}

/// Which colour palette the window uses over the light or dark base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaletteChoice {
    /// Calm water blues with duck yellow for what needs the user (the default).
    Pond,
    /// Violet blues.
    Dusk,
    /// Soft greens.
    Meadow,
    /// Warm reds and ambers.
    Ember,
    /// Neutral greys with a blue accent.
    Graphite,
    /// The user's own colours, Pond's where one is not set.
    Custom,
}

impl From<settings::PaletteChoice> for PaletteChoice {
    fn from(c: settings::PaletteChoice) -> Self {
        match c {
            settings::PaletteChoice::Pond => Self::Pond,
            settings::PaletteChoice::Dusk => Self::Dusk,
            settings::PaletteChoice::Meadow => Self::Meadow,
            settings::PaletteChoice::Ember => Self::Ember,
            settings::PaletteChoice::Graphite => Self::Graphite,
            settings::PaletteChoice::Custom => Self::Custom,
        }
    }
}

impl From<PaletteChoice> for settings::PaletteChoice {
    fn from(c: PaletteChoice) -> Self {
        match c {
            PaletteChoice::Pond => Self::Pond,
            PaletteChoice::Dusk => Self::Dusk,
            PaletteChoice::Meadow => Self::Meadow,
            PaletteChoice::Ember => Self::Ember,
            PaletteChoice::Graphite => Self::Graphite,
            PaletteChoice::Custom => Self::Custom,
        }
    }
}

/// What the close button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehaviour {
    /// Keep running in the tray.
    Tray,
    /// Quit Puddle.
    Quit,
}

impl From<settings::CloseBehaviour> for CloseBehaviour {
    fn from(c: settings::CloseBehaviour) -> Self {
        match c {
            settings::CloseBehaviour::Tray => Self::Tray,
            settings::CloseBehaviour::Quit => Self::Quit,
        }
    }
}

impl From<CloseBehaviour> for settings::CloseBehaviour {
    fn from(c: CloseBehaviour) -> Self {
        match c {
            CloseBehaviour::Tray => Self::Tray,
            CloseBehaviour::Quit => Self::Quit,
        }
    }
}

/// Preferences for Puddle's own window. `null` means Puddle's default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UiPrefs {
    /// Light, dark or follow the system (default `system`).
    #[serde(default)]
    #[schema(required = true)]
    pub theme: Option<ThemeChoice>,
    /// Comfortable or compact spacing for the whole app (default `comfortable`). A request may
    /// leave it out, like every field here: out or `null` means the default.
    #[serde(default)]
    #[schema(required = true)]
    pub density: Option<DensityChoice>,
    /// The colour palette over the light or dark base (default `pond`).
    #[serde(default)]
    #[schema(required = true)]
    pub palette: Option<PaletteChoice>,
    /// The `custom` palette's accent colour, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_accent: Option<String>,
    /// The `custom` palette's page background, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_background: Option<String>,
    /// The `custom` palette's cards and panels, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_surface: Option<String>,
    /// The `custom` palette's text, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_text: Option<String>,
    /// A system notification for a new request (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notifications: Option<bool>,
    /// The system sound with a notification (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub sound: Option<bool>,
    /// What the close button does (default `tray`).
    #[serde(default)]
    #[schema(required = true)]
    pub close_behaviour: Option<CloseBehaviour>,
    /// Minimizing hides the window to the tray instead of the taskbar (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub minimize_to_tray: Option<bool>,
    /// The icon of the window, the tray and the taskbar (default `duck`). `custom` needs an
    /// uploaded image.
    #[serde(default)]
    #[schema(required = true)]
    pub app_icon: Option<AppIconChoice>,
    /// Notify only while Puddle's window is not focused (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_only_unfocused: Option<bool>,
    /// The waiting count on the tray, taskbar and window title (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub badge: Option<bool>,
    /// Notify when a workspace is killed for running out of memory (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_oom: Option<bool>,
    /// Notify when a workspace's root disk or volume reaches `disk_warning_percent` (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_disk_full: Option<bool>,
    /// How full a workspace's root disk or volume gets, in percent, before Puddle warns on the
    /// workspace and, with `notify_disk_full`, notifies; 50 to 100 (default 95).
    #[serde(default)]
    #[schema(required = true, minimum = 50, maximum = 100)]
    pub disk_warning_percent: Option<u8>,
    /// Notify when a workspace stops or crashes without being asked to (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_unexpected_stop: Option<bool>,
    /// Ask before a decision creates a rule for every workspace (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub confirm_global_rules: Option<bool>,
    /// The inbox's single-key shortcuts A, D, J and K (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub inbox_shortcuts: Option<bool>,
    /// Start Puddle when the user signs in to the computer (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub start_at_login: Option<bool>,
    /// Notify when the free space on the drive that holds Puddle's data falls under the warning or
    /// the failure line (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_low_disk_space: Option<bool>,
    /// The free space, in GB (1024 x 1024 x 1024 bytes), under which `puddle doctor` warns and the
    /// app says so; 1 to 10000, default 20.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 10000)]
    pub disk_warn_free_gb: Option<u32>,
    /// The free space, in GB, under which `puddle doctor` fails; 1 to 10000, default 5, never above
    /// the warning line.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 10000)]
    pub disk_fail_free_gb: Option<u32>,
    /// The SSH agent `puddle doctor` asks for the key list: a named pipe on Windows (default
    /// `\\.\pipe\openssh-ssh-agent`), a socket path elsewhere (default the `SSH_AUTH_SOCK` of
    /// Puddle's session). Null or blank is unset; at most 1024 characters, no control characters.
    #[serde(default)]
    #[schema(required = true, max_length = 1024)]
    pub ssh_agent_pipe: Option<String>,
}

impl From<&settings::UiPrefs> for UiPrefs {
    fn from(u: &settings::UiPrefs) -> Self {
        Self {
            theme: u.theme.map(Into::into),
            density: u.density.map(Into::into),
            palette: u.palette.map(Into::into),
            custom_accent: colour_text(u.custom_accent.as_ref()),
            custom_background: colour_text(u.custom_background.as_ref()),
            custom_surface: colour_text(u.custom_surface.as_ref()),
            custom_text: colour_text(u.custom_text.as_ref()),
            notifications: u.notifications,
            sound: u.sound,
            close_behaviour: u.close_behaviour.map(Into::into),
            minimize_to_tray: u.minimize_to_tray,
            app_icon: u.app_icon.map(Into::into),
            notify_only_unfocused: u.notify_only_unfocused,
            badge: u.badge,
            notify_oom: u.notify_oom,
            notify_disk_full: u.notify_disk_full,
            disk_warning_percent: u
                .disk_warning_percent
                .map(settings::DiskWarningPercent::get),
            notify_unexpected_stop: u.notify_unexpected_stop,
            confirm_global_rules: u.confirm_global_rules,
            inbox_shortcuts: u.inbox_shortcuts,
            start_at_login: u.start_at_login,
            notify_low_disk_space: u.notify_low_disk_space,
            disk_warn_free_gb: u.disk_warn_free_gb.map(Into::into),
            disk_fail_free_gb: u.disk_fail_free_gb.map(Into::into),
            ssh_agent_pipe: u.ssh_agent_pipe.clone(),
        }
    }
}

pub(super) fn colour_text(colour: Option<&settings::HexColour>) -> Option<String> {
    colour.map(|c| c.as_str().to_owned())
}

fn free_gb(gb: Option<u32>) -> Result<Option<settings::FreeGb>, ApiError> {
    gb.map(settings::FreeGb::new)
        .transpose()
        .map_err(ApiError::invalid)
}

/// The longest pipe or socket path the settings take.
const SSH_AGENT_PIPE_MAX: usize = 1024;

/// The agent pipe or socket as stored: trimmed, a blank one unset.
fn agent_pipe_text(text: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(text) = text else { return Ok(None) };
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > SSH_AGENT_PIPE_MAX {
        return Err(ApiError::invalid(format!(
            "ssh_agent_pipe is longer than {SSH_AGENT_PIPE_MAX} characters"
        )));
    }
    if text.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "ssh_agent_pipe holds a control character",
        ));
    }
    Ok(Some(text.to_owned()))
}

fn parse_colour(text: Option<String>) -> Result<Option<settings::HexColour>, ApiError> {
    text.map(|t| settings::HexColour::new(&t))
        .transpose()
        .map_err(ApiError::invalid)
}

impl UiPrefs {
    pub(super) fn apply_to(self, prefs: &mut settings::UiPrefs) -> Result<(), ApiError> {
        let Self {
            theme,
            density,
            palette,
            custom_accent,
            custom_background,
            custom_surface,
            custom_text,
            notifications,
            sound,
            close_behaviour,
            minimize_to_tray,
            app_icon,
            notify_only_unfocused,
            badge,
            notify_oom,
            notify_disk_full,
            disk_warning_percent,
            notify_unexpected_stop,
            confirm_global_rules,
            inbox_shortcuts,
            start_at_login,
            notify_low_disk_space,
            disk_warn_free_gb,
            disk_fail_free_gb,
            ssh_agent_pipe,
        } = self;
        prefs.theme = theme.map(Into::into);
        prefs.density = density.map(Into::into);
        prefs.palette = palette.map(Into::into);
        prefs.custom_accent = parse_colour(custom_accent)?;
        prefs.custom_background = parse_colour(custom_background)?;
        prefs.custom_surface = parse_colour(custom_surface)?;
        prefs.custom_text = parse_colour(custom_text)?;
        prefs.notifications = notifications;
        prefs.sound = sound;
        prefs.close_behaviour = close_behaviour.map(Into::into);
        prefs.minimize_to_tray = minimize_to_tray;
        prefs.app_icon = app_icon.map(Into::into);
        prefs.notify_only_unfocused = notify_only_unfocused;
        prefs.badge = badge;
        prefs.notify_oom = notify_oom;
        prefs.notify_disk_full = notify_disk_full;
        prefs.disk_warning_percent = disk_warning_percent
            .map(settings::DiskWarningPercent::new)
            .transpose()
            .map_err(ApiError::invalid)?;
        prefs.notify_unexpected_stop = notify_unexpected_stop;
        prefs.confirm_global_rules = confirm_global_rules;
        prefs.inbox_shortcuts = inbox_shortcuts;
        prefs.start_at_login = start_at_login;
        prefs.notify_low_disk_space = notify_low_disk_space;
        prefs.disk_warn_free_gb = free_gb(disk_warn_free_gb)?;
        prefs.disk_fail_free_gb = free_gb(disk_fail_free_gb)?;
        prefs.ssh_agent_pipe = agent_pipe_text(ssh_agent_pipe)?;
        // The failure line above the warning line would make the warning unreachable.
        settings::DiskSpaceLimits::new(
            prefs
                .disk_warn_free_gb
                .unwrap_or(settings::FreeGb::DEFAULT_WARN),
            prefs
                .disk_fail_free_gb
                .unwrap_or(settings::FreeGb::DEFAULT_FAIL),
        )
        .map_err(ApiError::invalid)?;
        Ok(())
    }
}

/// Which level an effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// The workspace's override.
    Workspace,
    /// The global setting.
    Global,
    /// Puddle's built-in default.
    Default,
}

impl From<settings::Source> for SettingSource {
    fn from(source: settings::Source) -> Self {
        match source {
            settings::Source::Workspace => Self::Workspace,
            settings::Source::Global => Self::Global,
            settings::Source::Default => Self::Default,
        }
    }
}

/// An effective on/off value and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedBool {
    /// The value.
    pub value: bool,
    /// Where it came from.
    pub source: SettingSource,
}

/// An effective number (MiB or seconds, see the field) and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedNumber {
    /// The value.
    pub value: u32,
    /// Where it came from.
    pub source: SettingSource,
}

/// The effective clipboard-read setting and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedClipboardRead {
    /// The value.
    pub value: ClipboardRead,
    /// Where it came from.
    pub source: SettingSource,
}

fn rb(r: settings::Resolved<bool>) -> ResolvedBool {
    ResolvedBool {
        value: r.value,
        source: r.source.into(),
    }
}

/// The effective local toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EffectiveToggles {
    /// Loopback.
    pub loopback: ResolvedBool,
    /// Private networks.
    pub private: ResolvedBool,
    /// Link-local.
    pub link_local: ResolvedBool,
    /// Cloud metadata.
    pub metadata: ResolvedBool,
    /// Other special-purpose ranges.
    pub special: ResolvedBool,
    /// The company network.
    pub company_network: ResolvedBool,
}

/// The values a workspace gets: its override, else the global value, else Puddle's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EffectiveSettings {
    /// Guest memory in MiB.
    pub memory: ResolvedNumber,
    /// Guest vCPUs.
    pub cpus: ResolvedNumber,
    /// Local destination toggles.
    pub local_toggles: EffectiveToggles,
    /// Whether suffix rules reach local addresses.
    pub wildcards_reach_local: ResolvedBool,
    /// Reconnection grace in seconds.
    pub reconnection_grace: ResolvedNumber,
    /// Zoom hotkeys.
    pub zoom_hotkeys: ResolvedBool,
    /// Clipboard reads.
    pub clipboard_read: ResolvedClipboardRead,
    /// Whether direct SSH is on.
    pub direct_ssh: ResolvedBool,
    /// Whether logins made inside the workspace are captured.
    pub capture_logins: ResolvedBool,
    /// Whether a credential Puddle did not add raises a warning.
    pub warn_foreign_credentials: ResolvedBool,
}

impl From<settings::Effective> for EffectiveSettings {
    fn from(e: settings::Effective) -> Self {
        let settings::Effective {
            memory,
            cpus,
            local_toggles,
            wildcards_reach_local,
            reconnection_grace,
            zoom_hotkeys,
            clipboard_read,
            direct_ssh,
            capture_logins,
            warn_foreign_credentials,
        } = e;
        let settings::EffectiveToggles {
            loopback,
            private,
            link_local,
            metadata,
            special,
            company_network,
        } = local_toggles;
        Self {
            memory: ResolvedNumber {
                value: memory.value.get(),
                source: memory.source.into(),
            },
            cpus: ResolvedNumber {
                value: u32::from(cpus.value.get()),
                source: cpus.source.into(),
            },
            local_toggles: EffectiveToggles {
                loopback: rb(loopback),
                private: rb(private),
                link_local: rb(link_local),
                metadata: rb(metadata),
                special: rb(special),
                company_network: rb(company_network),
            },
            wildcards_reach_local: rb(wildcards_reach_local),
            reconnection_grace: ResolvedNumber {
                value: reconnection_grace.value.secs(),
                source: reconnection_grace.source.into(),
            },
            zoom_hotkeys: rb(zoom_hotkeys),
            clipboard_read: ResolvedClipboardRead {
                value: clipboard_read.value.into(),
                source: clipboard_read.source.into(),
            },
            direct_ssh: rb(direct_ssh),
            capture_logins: rb(capture_logins),
            warn_foreign_credentials: rb(warn_foreign_credentials),
        }
    }
}

/// The global settings, and what a workspace without overrides gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GlobalSettingsView {
    /// Defaults for every workspace.
    pub workspace_defaults: SettingsLayer,
    /// VS Code server options.
    pub vscode_server: VsCodeServer,
    /// Preferences for Puddle's window.
    pub ui: UiPrefs,
    /// The tool switches (global only).
    pub tools: GlobalTools,
    /// The effective values for a workspace with no overrides.
    pub effective: EffectiveSettings,
    /// Fields in the stored document this Puddle doesn't know (written by a newer one); they
    /// are kept.
    pub unknown_fields: Vec<String>,
}

/// New global settings. Replaces every value listed here; unknown stored fields are kept.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsRequest {
    /// Defaults for every workspace.
    #[serde(default)]
    pub workspace_defaults: SettingsLayer,
    /// VS Code server options.
    #[serde(default)]
    pub vscode_server: VsCodeServer,
    /// Preferences for Puddle's window.
    #[serde(default)]
    pub ui: UiPrefs,
    /// The tool switches.
    #[serde(default)]
    pub tools: GlobalTools,
}

impl GlobalSettingsRequest {
    pub(crate) fn apply_to(self, global: &mut settings::GlobalSettings) -> Result<(), ApiError> {
        let Self {
            workspace_defaults,
            vscode_server:
                VsCodeServer {
                    server,
                    telemetry,
                    auto_update,
                },
            ui,
            tools: GlobalTools { mise },
        } = self;
        global.tools.mise = mise;
        workspace_defaults.apply_to(&mut global.workspace_defaults)?;
        global.vscode_server.server = server.map(Into::into);
        global.vscode_server.telemetry = telemetry;
        global.vscode_server.auto_update = auto_update;
        ui.apply_to(&mut global.ui)
    }
}

impl GlobalSettingsView {
    pub(crate) fn new(loaded: &settings::Loaded<settings::GlobalSettings>) -> Self {
        let global = &loaded.settings;
        Self {
            workspace_defaults: (&global.workspace_defaults).into(),
            vscode_server: VsCodeServer {
                server: global.vscode_server.server.map(Into::into),
                telemetry: global.vscode_server.telemetry,
                auto_update: global.vscode_server.auto_update,
            },
            ui: (&global.ui).into(),
            tools: GlobalTools {
                mise: global.tools.mise,
            },
            effective: settings::resolve(global, None).into(),
            unknown_fields: loaded.unknown_fields.clone(),
        }
    }
}

/// One workspace's overrides and effective values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceSettingsView {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// Its overrides; `null` inherits.
    pub overrides: SettingsLayer,
    /// Its own tool switches; `null` is the default.
    pub tools: WorkspaceTools,
    /// What it gets.
    pub effective: EffectiveSettings,
    /// Unknown fields in its stored document (kept).
    pub unknown_fields: Vec<String>,
}

/// New overrides for one workspace. Replaces every value listed; `null` inherits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSettingsRequest {
    /// The overrides.
    #[serde(default)]
    pub overrides: SettingsLayer,
    /// Its tool switches.
    #[serde(default)]
    pub tools: WorkspaceTools,
}
