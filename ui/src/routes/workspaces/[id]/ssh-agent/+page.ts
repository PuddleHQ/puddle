// SPDX-License-Identifier: AGPL-3.0-or-later
import { redirect } from "@sveltejs/kit";
import { tabHref } from "#lib/workspaces/tabs.ts";
import type { PageLoad } from "./$types";

// This page moved under Settings; a bookmark or an old link still lands on it.
export const load: PageLoad = ({ params }) => {
  redirect(307, tabHref(params.id, "ssh-agent"));
};
