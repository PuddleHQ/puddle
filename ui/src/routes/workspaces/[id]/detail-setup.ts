// SPDX-License-Identifier: AGPL-3.0-or-later
// What every workspace detail test file does around its tests: reset the fakes and stores, load
// them, find a host by its text, open a tab. Import it after the file's vi.mock calls.
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach } from "vitest";
import { editorProblems } from "#lib/stores/editor-problems.svelte.ts";
import { pending } from "#lib/stores/pending.svelte.ts";
import { requestRulesStore } from "#lib/stores/request-rules.svelte.ts";
import { rulesStore } from "#lib/stores/rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import Settings from "./settings/+page.svelte";
import { goto, h, url } from "./detail-harness.ts";

const { inbox, api, workspace, rules, requestRules } = h;

// A host is drawn in pieces (HostName), so it is found by its whole text, without the port.
const hostMatcher = (host: string) => (_: string, el: Element | null) =>
  el?.classList.contains("host") === true &&
  el.textContent?.replace(/:\d+$/, "") === host;
export const findHost = (host: string) => screen.findByText(hostMatcher(host));
export const getHost = (host: string) => screen.getByText(hostMatcher(host));
export const queryHost = (host: string) =>
  screen.queryByText(hostMatcher(host));

/** The overrides a workspace has now (all inherited, for one never changed). */
export async function settingsOf(name: string) {
  const { data } = (await api.GET("/api/settings/workspaces/{workspace}", {
    params: { path: { workspace: name } },
  })) as unknown as { data: { overrides: Record<string, unknown> } };
  return data.overrides as never as (typeof api.overrides)[string];
}

function reset() {
  url.pathname = "/workspaces/w-demo";
  url.id = "w-demo";
  goto.mockReset();
  inbox.open = [];
  inbox.rules = [];
  inbox.calls = [];
  inbox.down = false;
  rules.down = false;
  api.list = [workspace("demo", { status: "running" })];
  api.checkouts = {};
  api.problems = [];
  editorProblems.problems = [];
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.check = null;
  api.overrides = {};
  api.tools = {};
  api.globalMise = null;
  api.globalEditor = {
    colour_theme: null,
    font_size: null,
    extensions: null,
    keybindings: null,
    settings: null,
  };
  api.miseAvailable = true;
  api.unknown = {};
  api.kept = {};
  api.interception = {};
  api.globalNeverDecrypt = [];
  api.learned = {};
  api.refuse.clear();
  rules.rules = [];
  rules.calls = [];
  pending.rows = [];
  pending.status = "loading";
  rulesStore.rules = [];
  rulesStore.status = "loading";
  requestRules.rules = [];
  requestRules.calls = [];
  requestRulesStore.rules = [];
  requestRulesStore.status = "loading";
  workspaces.list = [];
  workspaces.status = "loading";
  workspaces.progress = {};
  workspaces.oom = {};
  workspaceActions.connectOpen = false;
  workspaceActions.trustOpen = false;
  workspaceActions.deleteOpen = false;
  workspaceActions.deleting = null;
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
}

export function installReset() {
  beforeEach(reset);
  afterEach(cleanup);
}

export async function load() {
  await Promise.all([
    workspaces.refresh(),
    pending.refresh(),
    rulesStore.refresh(),
    requestRulesStore.refresh(),
  ]);
}

/** The Settings tab, once its table has loaded. */
export async function openSettings() {
  await load();
  render(Settings);
  await screen.findByText("Advanced");
}

/** The workspace Settings tab's table, once the Memory row has loaded. */
export async function openSettingsTab() {
  await load();
  render(Settings);
  return screen.findByLabelText("Memory") as Promise<HTMLSelectElement>;
}
