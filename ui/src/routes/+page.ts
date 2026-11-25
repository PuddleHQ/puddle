// SPDX-License-Identifier: AGPL-3.0-or-later
import { redirect } from "@sveltejs/kit";
import { api } from "#lib/api/client.ts";
import { STEPS, WELCOME_ROOT } from "#lib/welcome/steps.ts";

// The start screen: the first-run flow until it has been through, then the workspace list with
// a strip that links to the inbox when requests wait. When Puddle can't say whether the flow
// ran, the list is the safer start: it shows what is wrong with the service. A flow left half
// way resumes on the step the person was on.
export async function load(): Promise<never> {
  const state = await api.GET("/api/first-run").then(
    ({ data }) => data,
    () => undefined,
  );
  if (!state || state.completed) redirect(307, "/workspaces");
  redirect(307, STEPS.find((s) => s.id === state.step)?.href ?? WELCOME_ROOT);
}
