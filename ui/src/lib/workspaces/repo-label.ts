// SPDX-License-Identifier: AGPL-3.0-or-later
// A repository address in the short form a list shows: `owner/repo`. The full address stays one
// hover or one details page away; an address Puddle cannot read is shown as typed.
import { parseRepoUrl } from "#lib/identities/repo.ts";

export function repoLabel(url: string): string {
  const parsed = parseRepoUrl(url);
  return parsed.ok ? `${parsed.owner}/${parsed.repo}` : url;
}
