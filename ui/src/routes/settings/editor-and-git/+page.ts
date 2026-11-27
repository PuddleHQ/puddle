// SPDX-License-Identifier: AGPL-3.0-or-later
import { redirect } from "@sveltejs/kit";
import type { PageLoad } from "./$types";

// The Editor and Git tab became the Editor tab; a bookmark or an old link still lands on it.
export const load: PageLoad = () => {
  redirect(307, "/settings/editor");
};
