// SPDX-License-Identifier: AGPL-3.0-or-later
// The first-use questions of the filtered SSH agent: a workspace's `ssh` is logging in to a host
// the agent is on for, with a key no answer covers yet, and waits for the user. They sit in the
// inbox beside the requests. The list is read again on `ssh_agent_asks_changed` and on a resync.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";

export type SshAgentAsk = components["schemas"]["SshAgentAskView"];
export type SshAgentAnswer = components["schemas"]["SshAgentAnswerKind"];
export type Status = "loading" | "ready" | "failed";

type StoreApi = Pick<ApiClient, "GET" | "POST">;

const DOWN = "Puddle's service isn't answering.";

export class SshAgentAsksStore {
  asks = $state.raw<SshAgentAsk[]>([]);
  status = $state<Status>("loading");
  #ticket = 0;
  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;
  readonly #onCount: ((count: number) => void) | undefined;

  constructor(
    api: StoreApi = defaultApi,
    source: LiveSource | undefined = live,
    onCount: ((count: number) => void) | undefined = (n) => live.setAsks(n),
  ) {
    this.#api = api;
    this.#source = source;
    this.#onCount = onCount;
  }

  /** Reads the open questions. `quiet` keeps the page as it is while it does. */
  async load(quiet = false): Promise<void> {
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/ssh-agent-asks");
      if (ticket !== this.#ticket) return;
      if (data) {
        this.#set(data.asks);
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
      }
    } catch {
      if (ticket === this.#ticket && !quiet) this.status = "failed";
    }
  }

  /** Reads the list now and keeps it current; returns the function that stops listening. */
  start(): () => void {
    void this.load();
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (
          typeof event === "object" &&
          event !== null &&
          (event as { type?: unknown }).type === "ssh_agent_asks_changed"
        ) {
          void this.load(true);
        }
      },
      resync: () => void this.load(true),
    });
    return () => unsubscribe?.();
  }

  /**
   * Answers one question. A question that is gone (answered in another window, or dropped as the
   * workspace stopped) is not a failure: the list is read again and it is simply not there.
   */
  async answer(ask: SshAgentAsk, answer: SshAgentAnswer): Promise<Result> {
    try {
      const { response } = await this.#api.POST(
        "/api/workspaces/{id}/ssh-agent-asks/{ask}/answer",
        {
          params: { path: { id: ask.workspace, ask: ask.id } },
          body: { answer },
        },
      );
      if (!response.ok && response.status !== 404 && response.status !== 409) {
        return { ok: false, message: "Puddle couldn't take the answer." };
      }
      this.#set(this.asks.filter((a) => a.id !== ask.id));
      void this.load(true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  #set(asks: SshAgentAsk[]): void {
    this.asks = asks;
    this.#onCount?.(asks.length);
  }
}

export const sshAgentAsks = new SshAgentAsksStore();
