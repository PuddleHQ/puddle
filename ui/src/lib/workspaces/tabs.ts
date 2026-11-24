// SPDX-License-Identifier: AGPL-3.0-or-later
// The tabs of a workspace's page, in order. A tab is a folder under `routes/workspaces/[id]/`
// (the overview is the folder itself); a later tab is added here and as a folder.
export interface Tab {
  /** The path segment after the workspace's, empty for the overview. */
  slug: string;
  label: string;
}

export const TABS: readonly Tab[] = [
  { slug: "", label: "Overview" },
  { slug: "terminal", label: "Terminal" },
  { slug: "network", label: "Network" },
  { slug: "git", label: "Git" },
  { slug: "settings", label: "Settings" },
];

/**
 * The pages of the Settings tab, in the order of its list. The first is the table of settings
 * (the tab's own folder); the others are folders under `settings/`.
 */
export const SETTINGS_SECTIONS: readonly Tab[] = [
  { slug: "", label: "Settings" },
  { slug: "environment", label: "Environment" },
  { slug: "shell-init", label: "Shell init" },
  { slug: "ssh-agent", label: "SSH agent" },
  { slug: "ports", label: "Ports" },
];

const SECTION_SLUGS = new Set(
  SETTINGS_SECTIONS.map((s) => s.slug).filter(Boolean),
);

/** The path segments after the workspace's own, empty when the path is not under it. */
function segments(pathname: string, id: string): string[] {
  const prefix = `/workspaces/${encodeURIComponent(id)}`;
  const rest = pathname.startsWith(prefix) ? pathname.slice(prefix.length) : "";
  return rest.split("/").filter(Boolean);
}

/** The tab a path is on: `/workspaces/demo/network` is `network`; unknown is the overview. */
export function activeTab(pathname: string, id: string): string {
  const slug = segments(pathname, id)[0] ?? "";
  return TABS.some((t) => t.slug === slug) ? slug : "";
}

/** The Settings page a path is on: the table (empty) or one of the sections after it. */
export function activeSection(pathname: string, id: string): string {
  const [tab, section = ""] = segments(pathname, id);
  return tab === "settings" && SECTION_SLUGS.has(section) ? section : "";
}

/**
 * The address of a tab. A section of Settings (`environment`, `ports`, ...) is the address of
 * that page under the Settings tab.
 */
export function tabHref(id: string, slug: string): string {
  const base = `/workspaces/${encodeURIComponent(id)}`;
  if (slug === "") return base;
  return SECTION_SLUGS.has(slug)
    ? `${base}/settings/${slug}`
    : `${base}/${slug}`;
}
