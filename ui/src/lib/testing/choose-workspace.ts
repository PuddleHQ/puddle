// SPDX-License-Identifier: AGPL-3.0-or-later
import { fireEvent } from "@testing-library/svelte";
import { tick } from "svelte";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "./fake-workspaces.ts";

/** Picks workspace `id` in a workspace dropdown, listing it first as the live list would. */
export async function chooseWorkspace(select: HTMLElement, id: string) {
  if (!workspaces.byId(id)) {
    workspaces.list = [...workspaces.list, { ...fakeWorkspace(id), id }];
    await tick();
  }
  await fireEvent.change(select, { target: { value: id } });
}
