// SPDX-License-Identifier: AGPL-3.0-or-later
// What every inbox page test file does around its tests: reset the fakes and stores, seed
// requests, find rows and hosts. Import it after the file's vi.mock calls.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, vi } from "vitest";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import {
  FAKE_EFFECTIVE_LIMITS,
  FakeSettings,
} from "#lib/testing/fake-settings.ts";
import { pending, slices } from "#lib/stores/pending.svelte.ts";
import { sshAgentAsks } from "#lib/stores/ssh-agent-asks.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import InboxPage from "./+page.svelte";
import { h } from "./inbox-harness.ts";

const { inbox, setsApi, request, agentApi } = h;

export function reset() {
  globalSettings.view = null;
  inbox.open = [];
  inbox.rules = [];
  inbox.calls = [];
  inbox.down = false;
  inbox.openLimit = 1000;
  inbox.allowedToday = 0;
  pending.rows = [];
  pending.openLimit = Number.POSITIVE_INFINITY;
  pending.allowedToday = 0;
  pending.decided = [];
  workspaces.list = [];
  pending.status = "loading";
  setsApi.sets = [];
  agentApi.asks = [];
  agentApi.answers = [];
  agentApi.calls = [];
  agentApi.down = false;
  agentApi.nextStatus = null;
  sshAgentAsks.asks = [];
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
}

/**
 * Draws a long list in slices of 3 and 4 rows: the real sixty rows cost seconds on a busy host,
 * and no test here is about how many rows a slice holds.
 */

const realSlices = { ...slices };
export const shrinkSlices = () => Object.assign(slices, { first: 3, next: 4 });

export function installReset() {
  beforeEach(reset);
  afterEach(() => {
    vi.restoreAllMocks();
    Object.assign(slices, realSlices);
    cleanup();
    vi.useRealTimers();
  });
}

export function seed() {
  inbox.add(
    request(1, { host: "a.example.com", first_seen: 3_000 }),
    "example.com",
  );
  inbox.add(
    request(2, { host: "b.example.com", first_seen: 2_000 }),
    "example.com",
  );
  inbox.add(
    request(3, { host: "registry.other.org", first_seen: 1_000 }),
    "other.org",
  );
}

export async function ready() {
  const view = render(InboxPage);
  await screen.findByRole("heading", { name: "example.com" });
  return view;
}

// A role query with a name works out the accessible name of every candidate, and each of those reads
// the computed style of every node it passes: in jsdom the dialog's radios cost 5 ms apiece and the
// page's more-choices button 4 ms, a label query 0.5 ms. These tests are about what the choices do,
// so they look the radios and the more-choices button up by their label.
export const moreChoices = () =>
  screen.getByLabelText(/^More choices for a\.example\.com/);
export const choice = (dialog: HTMLElement, label: RegExp) =>
  within(dialog).getByLabelText(label);

// A host is drawn in pieces (HostName), so it is found by its whole text, without the port.
const hostMatcher = (host: string) => (_: string, el: Element | null) =>
  el?.classList.contains("host") === true &&
  el.textContent?.replace(/:\d+$/, "") === host;
export const hostEl = (host: string) => screen.getByText(hostMatcher(host));
export const queryHostEl = (host: string) =>
  screen.queryByText(hostMatcher(host));
export const rowOf = (host: string) =>
  hostEl(host).closest("li") as HTMLElement;
export const press = (key: string, target: Element = document.body) =>
  fireEvent.keyDown(target, { key });

export async function showOptions(): Promise<HTMLElement> {
  const dialog = await screen.findByRole("dialog", { hidden: true });
  if (dialog.parentElement) dialog.parentElement.style.visibility = "visible";
  return dialog;
}

/** The settings as the API sends them, with the inbox keys switched on or off. */
export function shortcuts(on: boolean | null) {
  const fake = new FakeSettings();
  fake.ui.inbox_shortcuts = on;
  globalSettings.view = {
    effective: {} as never,
    ui: fake.ui,
    unknown_fields: [],
    revision: fake.revision(),
    vscode_server: fake.vscode,
    tools: fake.tools,
    mise_available: true,
    limits: fake.limits,
    effective_limits: FAKE_EFFECTIVE_LIMITS,
    workspace_defaults: fake.layer,
  };
}

// A page's first render runs every component and store it uses for the first time (V8 compiles each
// function lazily, with coverage on it also counts), and the first role queries over its rows are the
// slowest ones. Unloaded that costs the first tests some tens of milliseconds, on a busy host it is
// seconds: it took the first test over its 5000 ms. Pay it here, outside any test's clock.
export async function warmUp() {
  seed();
  await ready();
  await fireEvent.click(moreChoices());
  const warmDialog = await showOptions();
  await fireEvent.click(choice(warmDialog, /^Everything under/));
  cleanup();
  reset();
}
