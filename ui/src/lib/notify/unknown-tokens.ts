// SPDX-License-Identifier: AGPL-3.0-or-later
// The notice for a token in an answer that Puddle does not hold: a website sent the workspace a
// credential of a kind Puddle knows (a sign-in a tool did on its own, a key in a page) and Puddle
// never saw it come. The answer went through unchanged; the notice says which website and what
// kind of token it looked like, never any part of the token. One notice for each workspace,
// website and kind.
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import type { components } from "#lib/api/schema.d.ts";
import type { Notifier } from "./notifier.ts";

type Event = components["schemas"]["Event"];
export type UnknownToken = Extract<Event, { type: "unknown_token_in_answer" }>;
type Kind = components["schemas"]["UnknownTokenKind"];

/** What each kind reads as in a sentence: "a GitHub token". */
export const KIND_NAME: Record<Kind, string> = {
  github_token: "a GitHub token",
  github_fine_grained_token: "a fine-grained GitHub token",
  gitlab_token: "a GitLab token",
  npm_token: "an npm token",
  azure_devops_token: "an Azure DevOps token",
  anthropic_key: "an Anthropic key",
  openai_key: "an OpenAI key",
  jwt_with_refresh_token: "a sign-in token with a refresh token",
};

const KINDS = Object.keys(KIND_NAME) as Kind[];

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/** The event when it is an unknown token with the right field types; anything else is ignored. */
export function asUnknownToken(raw: unknown): UnknownToken | null {
  if (!isObject(raw) || raw["type"] !== "unknown_token_in_answer") return null;
  const kind = raw["kind"];
  return typeof raw["workspace"] === "string" &&
    typeof raw["host"] === "string" &&
    typeof kind === "string" &&
    KINDS.includes(kind as Kind)
    ? (raw as UnknownToken)
    : null;
}

export const unknownTokenKey = (e: UnknownToken) =>
  `unknown-token:${e.workspace}:${e.host}:${e.kind}`;

export class UnknownTokenNotices {
  readonly #notifier: Notifier;

  constructor(notifier: Notifier) {
    this.#notifier = notifier;
  }

  /** Raises the notice an event calls for; false when the event is not one. */
  handle(raw: unknown): boolean {
    const event = asUnknownToken(raw);
    if (!event) return false;
    this.#notifier.notify({
      key: unknownTokenKey(event),
      tone: "warning",
      title: `${event.host} sent ${workspaces.labelOf(event.workspace)} ${KIND_NAME[event.kind]}.`,
      detail: `The answer went through as it was. Puddle did not add this token, so it can't swap it for a stand-in and the workspace now holds a real credential. If you expect it (a sign-in you started), nothing needs to change. You can turn this warning off in the workspace settings or in Settings.`,
      link: {
        href: `/workspaces/${encodeURIComponent(event.workspace)}/settings`,
        label: "Workspace settings",
      },
    });
    return true;
  }
}
