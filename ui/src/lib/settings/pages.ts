// SPDX-License-Identifier: AGPL-3.0-or-later
// The pages of Settings and every setting on them. The left list is built from `PAGES`, the search
// from `SETTINGS`; a test opens each page and checks that every entry's `id` is on it, so a setting
// cannot be added to a page and stay out of the search.

export type PageId =
  | "appearance"
  | "notifications"
  | "workspaces"
  | "network"
  | "editor"
  | "editor-and-git"
  | "this-computer";

export interface SettingsPage {
  id: PageId;
  label: string;
  /** The path of the page. */
  href: string;
}

const page = (id: PageId, label: string): SettingsPage => ({
  id,
  label,
  href: `/settings/${id}`,
});

export const PAGES: readonly SettingsPage[] = [
  page("appearance", "Appearance"),
  page("notifications", "Notifications"),
  page("workspaces", "Workspaces"),
  page("network", "Network"),
  page("editor", "Editor"),
  page("editor-and-git", "Editor and Git"),
  page("this-computer", "This computer"),
];

export interface SettingEntry {
  /** The element id of the setting on its page; the search links to `<page>#<id>`. */
  id: string;
  page: PageId;
  label: string;
  /** More words that should find it. */
  words: string;
}

const entry = (
  pageId: PageId,
  id: string,
  label: string,
  words = "",
): SettingEntry => ({ id, page: pageId, label, words });

export const SETTINGS: readonly SettingEntry[] = [
  entry("appearance", "set-theme", "Theme", "light dark system mode"),
  entry("appearance", "set-density", "Density", "compact comfortable spacing"),
  entry(
    "appearance",
    "set-palette",
    "Colours",
    "palette accent custom contrast",
  ),
  entry("appearance", "set-app-icon", "App icon", "duck logo image tray"),

  entry(
    "notifications",
    "set-notify",
    "System notifications for new requests",
    "desktop alert",
  ),
  entry(
    "notifications",
    "set-tab-notify",
    "Notifications in this browser",
    "tab permission",
  ),
  entry(
    "notifications",
    "set-unfocused",
    "Only when Puddle is not the window in front",
    "focus",
  ),
  entry("notifications", "set-sound", "Sound", "ding beep"),
  entry(
    "notifications",
    "set-badge",
    "Show the number of waiting requests",
    "count tray taskbar title",
  ),
  entry(
    "notifications",
    "set-oom",
    "A workspace runs out of memory",
    "oom killed",
  ),
  entry(
    "notifications",
    "set-disk-full",
    "A workspace's disk is nearly full",
    "volume root disk",
  ),
  entry(
    "notifications",
    "set-disk-percent",
    "Warn when a disk is this full (%)",
    "threshold percent",
  ),
  entry(
    "notifications",
    "set-stopped",
    "A workspace stops on its own",
    "crash stopped",
  ),
  entry(
    "notifications",
    "set-low-disk",
    "The drive that holds Puddle's data runs low on space",
    "free space",
  ),
  entry(
    "notifications",
    "set-confirm-global",
    "Confirm before a rule affects every workspace",
    "inbox global rule ask",
  ),
  entry(
    "notifications",
    "set-inbox-keys",
    "Keyboard shortcuts in the inbox",
    "keys hotkeys allow deny",
  ),

  entry("workspaces", "set-memory", "Default memory", "ram"),
  entry("workspaces", "set-cpus", "Default CPUs", "cores processors"),
  entry(
    "workspaces",
    "set-max-cpus",
    "Most CPUs a workspace may have",
    "cores limit",
  ),
  entry(
    "workspaces",
    "set-mise",
    "Manage tools with mise in workspaces",
    "tools versions",
  ),
  entry(
    "workspaces",
    "set-direct-ssh",
    "Direct SSH for new workspaces",
    "ssh port trust",
  ),
  entry(
    "workspaces",
    "set-capture",
    "Keep logins made in workspaces",
    "sign in token claude github copilot capture",
  ),
  entry(
    "workspaces",
    "set-foreign-warning",
    "Warn about a credential Puddle did not add",
    "token leak foreign",
  ),
  entry(
    "workspaces",
    "set-disk-warn",
    "Warn when free space is under (GB)",
    "drive disk space",
  ),
  entry(
    "workspaces",
    "set-disk-fail",
    "Fail when free space is under (GB)",
    "drive disk space",
  ),
  entry(
    "workspaces",
    "set-env",
    "Environment of every workspace",
    "variables secrets env",
  ),

  entry(
    "network",
    "set-net-health",
    "Network health",
    "proxy certificates company",
  ),
  entry(
    "network",
    "set-proxy-sign-in",
    "Proxy sign-in",
    "password user name proxy",
  ),
  entry(
    "network",
    "set-local-loopback",
    "This computer (loopback)",
    "local destinations localhost 127.0.0.1",
  ),
  entry(
    "network",
    "set-local-private",
    "Private networks",
    "local destinations lan 192.168 10.",
  ),
  entry(
    "network",
    "set-local-company_network",
    "Company network",
    "local destinations vpn",
  ),
  entry(
    "network",
    "set-local-link_local",
    "Link-local addresses",
    "local destinations 169.254",
  ),
  entry(
    "network",
    "set-local-metadata",
    "Instance metadata",
    "local destinations cloud",
  ),
  entry(
    "network",
    "set-local-special",
    "Special-use ranges",
    "local destinations cgnat",
  ),
  entry(
    "network",
    "set-wild",
    "Let suffix rules reach local addresses",
    "wildcard subdomain",
  ),
  entry(
    "network",
    "set-interception-mode",
    "Which hosts to decrypt",
    "interception tls mitm decrypt",
  ),
  entry(
    "network",
    "set-interception-learn",
    "Learn from rejections",
    "interception learned pinned",
  ),
  entry(
    "network",
    "set-never-decrypt",
    "Names no workspace decrypts",
    "never decrypt hosts list",
  ),
  entry(
    "network",
    "set-learned-limits",
    "Hosts left alone",
    "learned limits minutes remembered",
  ),

  entry(
    "editor",
    "set-editor-theme",
    "Editor colour theme",
    "editor vs code theme dark light colour color",
  ),
  entry(
    "editor",
    "set-editor-font-size",
    "Font size (pixels)",
    "editor vs code text size zoom",
  ),
  entry(
    "editor",
    "set-editor-extensions",
    "Extensions to install",
    "editor vs code plugins preinstall",
  ),
  entry(
    "editor",
    "set-editor-keybindings",
    "Key bindings",
    "editor vs code keyboard shortcuts keymap",
  ),

  entry(
    "editor-and-git",
    "set-server",
    "Server",
    "code-server microsoft vs code browser",
  ),
  entry(
    "editor-and-git",
    "set-ms-telemetry",
    "Send Microsoft telemetry",
    "microsoft",
  ),
  entry(
    "editor-and-git",
    "set-update",
    "Update the server automatically",
    "auto update",
  ),
  entry(
    "editor-and-git",
    "set-grace",
    "Reconnection grace (seconds)",
    "session reconnect",
  ),
  entry(
    "editor-and-git",
    "set-zoom",
    "Zoom shortcuts",
    "ctrl plus minus hotkeys",
  ),
  entry(
    "editor-and-git",
    "set-clip",
    "Clipboard reads by pages",
    "paste clipboard",
  ),
  entry(
    "editor-and-git",
    "set-ssh-agent",
    "SSH agent pipe or socket",
    "pageant 1password openssh keys",
  ),
  entry(
    "editor-and-git",
    "set-identities",
    "Git and credentials",
    "identities name email commit",
  ),

  entry(
    "this-computer",
    "set-login",
    "Start Puddle when I sign in",
    "startup autostart login boot",
  ),
  entry("this-computer", "set-close", "Closing the window", "tray quit exit"),
  entry(
    "this-computer",
    "set-minimize",
    "Minimizing hides Puddle in the tray",
    "taskbar",
  ),
  entry(
    "this-computer",
    "set-devices",
    "Devices and sign-in",
    "password phones browsers sessions",
  ),
  entry(
    "this-computer",
    "set-backup",
    "Back up and restore",
    "export archive restore",
  ),
  entry(
    "this-computer",
    "set-audit",
    "Activity log",
    "retention days audit history delete",
  ),
  entry(
    "this-computer",
    "set-system-check",
    "System check",
    "virtualization hypervisor kvm whp",
  ),
  entry(
    "this-computer",
    "set-app-errors",
    "App errors",
    "log crash page errors",
  ),
  entry(
    "this-computer",
    "set-privacy",
    "Privacy",
    "telemetry analytics update check",
  ),
  entry(
    "this-computer",
    "set-about",
    "About",
    "version licence licences third-party",
  ),
];

/** The page an old `/settings#s-...` anchor now lives on, with the anchor to scroll to. */
export const LEGACY_ANCHORS: Readonly<Record<string, string>> = {
  "s-app": "appearance#set-theme",
  "s-icon": "appearance#set-app-icon",
  "s-startup": "this-computer#set-login",
  "s-notify": "notifications#set-notify",
  "s-inbox": "notifications#set-confirm-global",
  "s-ws": "workspaces#set-memory",
  "s-disk": "workspaces#set-disk-warn",
  "s-audit": "this-computer#set-audit",
  "s-ssh": "editor-and-git#set-ssh-agent",
  "s-net": "network#set-net-health",
  "local-destinations": "network#set-local-loopback",
  "s-vsc": "editor-and-git#set-server",
  "s-git": "editor-and-git#set-identities",
  "s-env": "workspaces#set-env",
  "s-dev": "this-computer#set-devices",
  "s-backup": "this-computer#set-backup",
  "s-priv": "this-computer#set-privacy",
  "s-about": "this-computer#set-about",
};

/** Where an old anchor goes, as a path; the first page when the anchor is not known. */
export function legacyTarget(hash: string): string {
  const target = LEGACY_ANCHORS[hash.replace(/^#/, "")];
  return `/settings/${target ?? "appearance"}`;
}

const norm = (text: string) => text.toLowerCase();

/** The settings whose name or words contain every word typed, in list order. */
export function searchSettings(query: string): SettingEntry[] {
  const terms = norm(query).split(/\s+/).filter(Boolean);
  if (terms.length === 0) return [];
  return SETTINGS.filter((s) => {
    const hay = norm(`${s.label} ${s.words} ${pageLabel(s.page)}`);
    return terms.every((t) => hay.includes(t));
  });
}

export const pageLabel = (id: PageId): string =>
  PAGES.find((p) => p.id === id)?.label ?? id;

/** The path a search result opens. */
export const settingHref = (s: SettingEntry): string =>
  `/settings/${s.page}#${s.id}`;
