// SPDX-License-Identifier: GPL-3.0-or-later
// The host's problems about a workspace's browser editor server: it could not be installed
// (`editor-install:<workspace>`, told on the workspace's page and in the Connect dialog) or could
// not take a newer build (`editor-update:<workspace>`, the page only: the editor still opens).
// Read again when the host says its problem list changed.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import {
  editorInstallProblemOf,
  editorUpdateProblemOf,
  type Problem,
} from "#lib/workspaces/problem-links.ts";
import { live, type LiveSource } from "./live.svelte.ts";

export class EditorProblemsStore {
  problems = $state.raw<Problem[]>([]);
  #ticket = 0;
  readonly #api: Pick<ApiClient, "GET">;
  readonly #source: LiveSource | undefined;

  constructor(
    api: Pick<ApiClient, "GET"> = defaultApi,
    source: LiveSource | undefined = live,
  ) {
    this.#api = api;
    this.#source = source;
  }

  /** What `workspace`'s install problem says, `null` when the host lists none. */
  of(workspace: string): Problem | null {
    return editorInstallProblemOf(this.problems, workspace) ?? null;
  }

  /** What `workspace`'s update problem says, `null` when the host lists none. */
  updateOf(workspace: string): Problem | null {
    return editorUpdateProblemOf(this.problems, workspace) ?? null;
  }

  /** Reads the list. A failed read leaves what the page shows as it is. */
  async load(): Promise<void> {
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/problems");
      if (ticket === this.#ticket && data) this.problems = data.problems;
    } catch {
      // The line stays as it was.
    }
  }

  /** Reads now and follows the host's changes; returns the function that stops listening. */
  start(): () => void {
    void this.load();
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if ((event as { type?: unknown } | null)?.type === "problems_changed")
          void this.load();
      },
      resync: () => void this.load(),
    });
    return () => unsubscribe?.();
  }
}

export const editorProblems = new EditorProblemsStore();
