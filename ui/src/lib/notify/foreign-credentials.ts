// SPDX-License-Identifier: AGPL-3.0-or-later
// The notice for a credential Puddle did not add: a tool in a workspace sent a token of its own to
// a website. The request went through as it always did; the notice says to which website and what
// kind of token it looked like, never any part of the token. One notice for each workspace,
// website and kind. The wording is in plain words: Puddle can't tell the user's own token from
// one that leaked, so it never says "leaked".
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import type { components } from "#lib/api/schema.d.ts";
import type { Notifier } from "./notifier.ts";

type Event = components["schemas"]["Event"];
export type ForeignCredential = Extract<Event, { type: "foreign_credential" }>;
type Kind = components["schemas"]["ForeignCredentialKind"];

/** What each kind reads as in a sentence: "a GitHub token". */
export const KIND_NAME: Record<Kind, string> = {
  stale_stand_in: "a stand-in Puddle no longer holds",
  github_token: "a GitHub token",
  npm_token: "an npm token",
  gitlab_token: "a GitLab token",
  azure_devops_token: "an Azure DevOps token",
  git_authorization: "a login for a Git repository",
};

const KINDS = Object.keys(KIND_NAME) as Kind[];

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/** The event when it is a foreign credential with the right field types; anything else is ignored. */
export function asForeignCredential(raw: unknown): ForeignCredential | null {
  if (!isObject(raw) || raw["type"] !== "foreign_credential") return null;
  const kind = raw["kind"];
  return typeof raw["workspace"] === "string" &&
    typeof raw["host"] === "string" &&
    typeof kind === "string" &&
    KINDS.includes(kind as Kind)
    ? (raw as ForeignCredential)
    : null;
}

export const foreignCredentialKey = (e: ForeignCredential) =>
  `foreign-credential:${e.workspace}:${e.host}:${e.kind}`;

function detailFor(e: ForeignCredential): string {
  if (e.kind === "stale_stand_in") {
    return `A tool is using a stand-in that Puddle made once and no longer holds (a secret or login that was removed, or one from another workspace). Puddle can't swap it for the real value, so ${e.host} will refuse it. The request went out as it was.`;
  }
  return `The request went out as it was. Puddle did not add this credential and can't tell whether it is yours or one that ended up somewhere it should not be. If it is yours and you expect it, nothing needs to change. You can turn this warning off in the workspace settings or in Settings.`;
}

export class ForeignCredentialNotices {
  readonly #notifier: Notifier;

  constructor(notifier: Notifier) {
    this.#notifier = notifier;
  }

  /** Raises the notice an event calls for; false when the event is not one. */
  handle(raw: unknown): boolean {
    const event = asForeignCredential(raw);
    if (!event) return false;
    this.#notifier.notify({
      key: foreignCredentialKey(event),
      tone: "warning",
      title: `${workspaces.labelOf(event.workspace)} sent ${KIND_NAME[event.kind]} to ${event.host}.`,
      detail: detailFor(event),
      link: {
        href: `/workspaces/${encodeURIComponent(event.workspace)}/settings`,
        label: "Workspace settings",
      },
    });
    return true;
  }
}
