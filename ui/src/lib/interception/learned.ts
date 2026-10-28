// SPDX-License-Identifier: GPL-3.0-or-later
// The hosts Puddle stopped decrypting because a tool in a workspace refused Puddle's certificate:
// the event that announces one, the call that decrypts a host again, and the notice that offers it.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { sentence } from "#lib/rules/model.ts";
import type { Notifier } from "#lib/notify/notifier.ts";
import type { NoticeAction } from "#lib/notify/notices.svelte.ts";

type Event = components["schemas"]["Event"];
export type PassThroughLearned = Extract<
  Event,
  { type: "pass_through_learned" }
>;

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/** The event with the fields it must have, or `null` for anything else. */
export function asPassThroughLearned(raw: unknown): PassThroughLearned | null {
  return isObject(raw) &&
    raw["type"] === "pass_through_learned" &&
    typeof raw["workspace"] === "string" &&
    typeof raw["host"] === "string"
    ? (raw as PassThroughLearned)
    : null;
}

/** How long a learned host has left, as a person would say it. */
export function timeLeft(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  if (minutes < 1) return "less than a minute";
  const hours = Math.floor(minutes / 60);
  const unit = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
  if (hours < 1) return unit(minutes, "minute");
  const rest = minutes % 60;
  return rest === 0
    ? unit(hours, "hour")
    : `${unit(hours, "hour")} ${unit(rest, "minute")}`;
}

const decryptedAgain = new Set<(workspace: string) => void>();

/** Calls `listener` with the workspace when a host there was decrypted again from this window. */
export function onDecryptedAgain(
  listener: (workspace: string) => void,
): () => void {
  decryptedAgain.add(listener);
  return () => decryptedAgain.delete(listener);
}

/**
 * Makes Puddle decrypt `host` again from the next connection. A failure throws the API's own
 * words. A host that is no longer listed (expired, or decrypted again from another window) counts
 * as done: what the click wanted is true.
 */
export async function decryptAgain(
  api: Pick<ApiClient, "DELETE">,
  workspace: string,
  host: string,
): Promise<void> {
  let result;
  try {
    result = await api.DELETE(
      "/api/workspaces/{id}/interception/learned/{host}",
      { params: { path: { id: workspace, host } } },
    );
  } catch {
    throw new Error("Puddle's service isn't answering. Try again.");
  }
  if (result.response.ok || result.response.status === 404) {
    for (const listener of decryptedAgain) listener(workspace);
    return;
  }
  throw new Error(
    result.error?.message
      ? sentence(result.error.message)
      : "Puddle couldn't decrypt the host again.",
  );
}

export const learnedKey = (workspace: string, host: string) =>
  `learned:${workspace}:${host}`;

export class LearnedNotices {
  readonly #notifier: Notifier;
  readonly #api: Pick<ApiClient, "DELETE">;

  constructor(notifier: Notifier, api: Pick<ApiClient, "DELETE"> = defaultApi) {
    this.#notifier = notifier;
    this.#api = api;
  }

  #again(e: PassThroughLearned): NoticeAction {
    return {
      label: "Decrypt again",
      run: async () => {
        await decryptAgain(this.#api, e.workspace, e.host);
        this.#notifier.resolve(learnedKey(e.workspace, e.host));
      },
    };
  }

  /** Raises the notice for a learned host; false when the event is not one. */
  handle(raw: unknown): boolean {
    const event = asPassThroughLearned(raw);
    if (!event) return false;
    this.#notifier.notify({
      key: learnedKey(event.workspace, event.host),
      tone: "info",
      title: `Puddle stopped decrypting ${event.host}: its client refused Puddle's certificate.`,
      detail: `In ${event.workspace}. A tool there has its own list of trusted roots or pins the server's certificate, so Puddle passes the host through untouched for a while. Decrypting it again only works once the tool trusts Puddle's certificate.`,
      link: {
        href: `/workspaces/${encodeURIComponent(event.workspace)}/settings`,
        label: "Workspace settings",
      },
      action: this.#again(event),
    });
    return true;
  }
}
