// SPDX-License-Identifier: AGPL-3.0-or-later
// The hosts Puddle stopped decrypting because a tool in a workspace refused Puddle's certificate:
// the event that announces one, the calls that decrypt a host again or make it permanent, and the
// notice that suggests making it permanent once a tool keeps refusing.
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
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
    typeof raw["host"] === "string" &&
    typeof raw["count"] === "number"
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

/** How long ago `at` (epoch ms) was at `now`, in the biggest sensible unit. */
export function howLongAgo(at: number, now: number = Date.now()): string {
  const minutes = Math.floor(Math.max(0, now - at) / 60_000);
  if (minutes < 1) return "just now";
  const unit = (n: number, word: string) =>
    `${n} ${word}${n === 1 ? "" : "s"} ago`;
  if (minutes < 60) return unit(minutes, "minute");
  const hours = Math.floor(minutes / 60);
  return hours < 48 ? unit(hours, "hour") : unit(Math.floor(hours / 24), "day");
}

/** How many refusals, in words. */
export function refusals(count: number): string {
  return count === 1 ? "refused once" : `refused ${count} times`;
}

/** From this many refusals on the app suggests making the host permanent. */
export const NOTICE_FROM_COUNT = 3;

const changed = new Set<(workspace: string) => void>();

/** Calls `listener` with the workspace when its learned list was changed from this window. */
export function onLearnedChanged(
  listener: (workspace: string) => void,
): () => void {
  changed.add(listener);
  return () => changed.delete(listener);
}

/** Tells the listeners that `workspace`'s learned list or never-decrypt names changed. */
export function announceLearnedChange(workspace: string): void {
  for (const listener of changed) listener(workspace);
}

type Scope = components["schemas"]["PermanentScope"];

/**
 * Makes Puddle decrypt `host` again from the next connection. A failure throws the API's own
 * words. A host that is no longer passed through (expired, or decrypted again from another
 * window) counts as done: what the click wanted is true.
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
    announceLearnedChange(workspace);
    return;
  }
  throw new Error(
    result.error?.message
      ? sentence(result.error.message)
      : "Puddle couldn't decrypt the host again.",
  );
}

/**
 * Puts `host` on the never-decrypt names of the workspace or of every workspace and takes it off
 * the learned list. A failure throws the API's own words.
 */
export async function makePermanent(
  api: Pick<ApiClient, "POST">,
  workspace: string,
  host: string,
  scope: Scope,
): Promise<void> {
  let result;
  try {
    result = await api.POST(
      "/api/workspaces/{id}/interception/learned/{host}/permanent",
      { params: { path: { id: workspace, host } }, body: { scope } },
    );
  } catch {
    throw new Error("Puddle's service isn't answering. Try again.");
  }
  if (result.response.ok) {
    announceLearnedChange(workspace);
    return;
  }
  throw new Error(
    result.error?.message
      ? sentence(result.error.message)
      : "Puddle couldn't make the host permanent.",
  );
}

export const learnedKey = (workspace: string, host: string) =>
  `learned:${workspace}:${host}`;

export class LearnedNotices {
  readonly #notifier: Notifier;
  readonly #api: Pick<ApiClient, "POST">;

  constructor(notifier: Notifier, api: Pick<ApiClient, "POST"> = defaultApi) {
    this.#notifier = notifier;
    this.#api = api;
  }

  #permanent(e: PassThroughLearned): NoticeAction {
    return {
      label: "Never decrypt it here",
      run: async () => {
        await makePermanent(this.#api, e.workspace, e.host, "workspace");
        this.#notifier.resolve(learnedKey(e.workspace, e.host));
      },
    };
  }

  /**
   * Raises the notice for a learned host once a tool has refused it `NOTICE_FROM_COUNT` times;
   * the earlier refusals only show in the workspace's list. False when the event is not one.
   */
  handle(raw: unknown): boolean {
    const event = asPassThroughLearned(raw);
    if (!event) return false;
    if (event.count < NOTICE_FROM_COUNT) return true;
    this.#notifier.notify({
      key: learnedKey(event.workspace, event.host),
      tone: "info",
      title: `A tool keeps refusing Puddle's certificate for ${event.host}: Puddle has stopped decrypting it ${event.count} times.`,
      detail: `In ${workspaces.labelOf(event.workspace)}. A tool there has its own list of trusted roots or pins the server's certificate, so Puddle passes the host through untouched for a while, and the tool refuses again each time Puddle tries. Making it permanent stops Puddle trying; you can undo that in the workspace's settings.`,
      link: {
        href: `/workspaces/${encodeURIComponent(event.workspace)}/settings`,
        label: "Workspace settings",
      },
      action: this.#permanent(event),
    });
    return true;
  }
}
