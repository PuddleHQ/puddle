// SPDX-License-Identifier: AGPL-3.0-or-later
// Reading `params` in the `$app/state` mock is tracked, so a test can change the id the way a
// navigation does. This file must load after the mocks (the mock factory imports it), not in a
// `vi.hoisted` call: the map has to come from the same Svelte runtime the components use.
import { SvelteMap } from "svelte/reactivity";
import { url } from "./detail-harness.ts";

export const navigations = new SvelteMap([["n", 0]]);

export function navigate(id: string) {
  url.id = id;
  navigations.set("n", (navigations.get("n") ?? 0) + 1);
}
