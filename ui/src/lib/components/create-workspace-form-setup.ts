// SPDX-License-Identifier: AGPL-3.0-or-later
// What every CreateWorkspaceForm test file does around its tests: clean up after each test and
// mount the form. Import it after the file's vi.mock calls.
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import type { ActionResult } from "#lib/stores/workspaces.svelte.ts";
import type { Workspace } from "#lib/workspaces/model.ts";
import { clearDraft } from "#lib/workspaces/draft.ts";
import { session } from "#lib/session/session.svelte.ts";
import CreateWorkspaceForm from "./CreateWorkspaceForm.svelte";
import { h } from "./create-workspace-form-harness.ts";

export function installReset() {
  afterEach(() => {
    clearDraft();
    h.desktop = false;
    session.mode = "checking";
    session.user = null;
    h.pick.mockReset();
    cleanup();
    for (const t of [...toasts.items]) toasts.dismiss(t.id);
  });
}

export function mount(
  result: ActionResult<Workspace> | (() => Promise<ActionResult<Workspace>>) = {
    ok: true,
    value: workspace("made", { busy: "creating", status: "created" }),
  },
) {
  const create = vi.fn(async (_body: unknown) =>
    typeof result === "function" ? result() : result,
  );
  const onCreated = vi.fn();
  render(CreateWorkspaceForm, {
    props: { store: { create }, onCreated, settings: settings() },
  } as never);
  return { create, onCreated };
}

/** The global settings as far as the form reads them: the limits, or none before they load. */
export function settings(
  limits: { max_cpus: number; host_cpus: number } | null = null,
  byDefault = 4,
  memoryMib = 8192,
) {
  return {
    load: vi.fn(async () => {}),
    view: {
      effective_limits: limits && { ...limits, default_cpus: byDefault },
      effective: {
        cpus: { value: byDefault, source: "default" },
        memory: { value: memoryMib, source: "default" },
      },
    },
  } as never;
}

export function mountWithLimits(
  limits: { max_cpus: number; host_cpus: number } | null,
  byDefault = 4,
  result: ActionResult<Workspace> = {
    ok: true,
    value: workspace("made", { busy: "creating", status: "created" }),
  },
) {
  const create = vi.fn(async (_body: unknown) => result);
  const source = settings(limits, byDefault) as { load: () => void };
  render(CreateWorkspaceForm, {
    props: { store: { create }, settings: source },
  } as never);
  return { create, source };
}

export const field = (label: string) =>
  screen.getByLabelText(label) as HTMLInputElement;
export const submit = () =>
  screen.getByRole("button", { name: /Create workspace|Creating/ });
export const type = (label: string, value: string) =>
  fireEvent.input(field(label), { target: { value } });
