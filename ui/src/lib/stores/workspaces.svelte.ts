// SPDX-License-Identifier: AGPL-3.0-or-later
/* eslint-disable svelte/prefer-svelte-reactivity -- the counter map here is bookkeeping that no template reads, so it needs no reactivity */
// The workspace screens' state: the list, what each one is doing (progress steps), the last
// out-of-memory kill, and the actions (create, start, stop, reclaim, attach, delete).
//
// Live data: it listens to the shell's event stream and applies `status_changed`,
// `workspace_progress` and `oom_kill` in place. Events that arrive while a refetch is running are
// applied after it, so a stale answer can't undo them. A resync (`lagged`, reconnect) refetches,
// and a slow poll covers anything else.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import {
  asWorkspaceEvent,
  type WorkspaceEvent,
} from "#lib/workspaces/events.ts";
import {
  operationOfStep,
  sortWorkspaces,
  type DeleteCheck,
  type Operation,
  type Step,
  type Workspace,
} from "#lib/workspaces/model.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type NewWorkspace = components["schemas"]["NewWorkspaceRequest"];
export type AttachMode = components["schemas"]["AttachMode"];
export type Attached = components["schemas"]["AttachResponse"];

export type Status = "loading" | "ready" | "failed";

/** What a workspace is doing, or just failed to do, as the stream last said. */
export interface Progress {
  step: Step;
  /** More about the step, or why it failed. Can quote tool output; only ever shown as text. */
  detail: string | null;
  /** The checkout directory a cloning step is about; `null` for any other step. */
  repository: string | null;
  /** The operation it belongs to, when the record said (`busy`). */
  operation: Operation | null;
  failed: boolean;
}

export interface Settled {
  /** The workspace's id, which events and every call use. */
  id: string;
  /** What the user calls it (its display name), for words; `id` when it is no longer listed. */
  label: string;
  operation: Operation | null;
  failed: boolean;
  detail: string | null;
}

export interface OomEvent {
  process: string;
  pid: number;
  /** Epoch ms the page heard of it. */
  at: number;
}

/** `field` tells a form where to put the message; `form` is anything else. */
export type Field =
  "name" | "repo_url" | "image" | "memory_mib" | "start_folder" | "form";

export type ActionResult<T = null> =
  | { ok: true; value: T }
  | { ok: false; message: string; field?: Field; reason?: "gone" | "conflict" };

type StoreApi = Pick<ApiClient, "GET" | "POST" | "PUT" | "PATCH" | "DELETE">;

export interface WorkspacesDeps {
  api?: StoreApi;
  source?: LiveSource;
  now?: () => number;
  /** Safety refresh when no event arrives. */
  pollMs?: number;
}

const DOWN = "Puddle's service isn't answering.";

/** Which create-form field a refusal is about, from its text. */
export function fieldOf(message: string): Field {
  const m = message.toLowerCase();
  if (/start folder|start_folder/.test(m)) return "start_folder";
  if (/ssh|repository|repositories|url|https/.test(m)) return "repo_url";
  if (/image/.test(m)) return "image";
  if (/memory/.test(m)) return "memory_mib";
  if (/name|already exists/.test(m)) return "name";
  return "form";
}

export class WorkspaceStore {
  list = $state.raw<Workspace[]>([]);
  status = $state<Status>("loading");
  /** Why the last read of the list failed; `null` once a read works. */
  loadError = $state<string | null>(null);
  progress = $state.raw<Record<string, Progress>>({});
  oom = $state.raw<Record<string, OomEvent>>({});

  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;
  readonly #now: () => number;
  readonly #pollMs: number;
  #buffer: WorkspaceEvent[] | null = null;
  /** How many operations of each workspace have ended; tells a response from a stale one. */
  readonly #ended = new Map<string, number>();
  #refreshing: Promise<void> | null = null;
  #again = false;
  /** Called when an operation on a workspace finished (`done`) or failed. */
  onSettled: ((settled: Settled) => void) | null = null;

  constructor(deps: WorkspacesDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#source = deps.source;
    this.#now = deps.now ?? Date.now;
    this.#pollMs = deps.pollMs ?? 60_000;
  }

  byId(id: string): Workspace | undefined {
    return this.list.find((w) => w.id === id);
  }

  /** The words for a workspace: its display name, or the id when it is not listed (deleted). */
  labelOf(id: string): string {
    return this.byId(id)?.name ?? id;
  }

  /**
   * `labelOf` for a sentence a person hears (an accessible name, a toast): the display name alone,
   * and only when another listed workspace shares it, followed by the id so the two can be told apart.
   */
  spokenLabelOf(id: string): string {
    const name = this.labelOf(id);
    const clash = this.list.some((w) => w.id !== id && w.name === name);
    return clash ? `${name} (${id})` : name;
  }

  /** Reads the list; never throws. Calls that overlap share one run, and one more follows. */
  refresh(): Promise<void> {
    if (this.#refreshing) {
      this.#again = true;
      return this.#refreshing;
    }
    this.#buffer = [];
    this.#refreshing = this.#load().finally(() => {
      const buffered = this.#buffer ?? [];
      this.#buffer = null;
      this.#refreshing = null;
      for (const event of buffered) this.#apply(event);
      if (this.#again) {
        this.#again = false;
        void this.refresh();
      }
    });
    return this.#refreshing;
  }

  async #load(): Promise<void> {
    try {
      const { data, error, response } = await this.#api.GET("/api/workspaces");
      if (data) {
        this.list = sortWorkspaces(data.workspaces);
        this.loadError = null;
        this.status = "ready";
      } else {
        this.loadError = readFailure(error, response);
        if (this.status === "loading") this.status = "failed";
      }
    } catch {
      this.loadError = SERVICE_DOWN;
      if (this.status === "loading") this.status = "failed";
    }
  }

  #put(next: Workspace): void {
    const others = this.list.filter((w) => w.id !== next.id);
    this.list = sortWorkspaces([...others, next]);
  }

  /** Applies one stream event in place. Anything the workspace screens don't use is ignored. */
  handleEvent(raw: unknown): void {
    const event = asWorkspaceEvent(raw);
    if (!event) return;
    if (this.#buffer) this.#buffer.push(event);
    else this.#apply(event);
  }

  #apply(event: WorkspaceEvent): void {
    const id = event.workspace;
    switch (event.type) {
      case "status_changed": {
        const w = this.byId(id);
        if (!w) {
          // Created somewhere else (the command line): read it.
          void this.refresh();
          return;
        }
        this.#put({ ...w, status: event.status });
        return;
      }
      case "workspace_repositories_changed":
        // The list of repositories is part of the record: read it again.
        void this.refresh();
        return;
      case "disk_nearly_full":
        // The figures and the warning flags are part of the record: read it again.
        void this.refresh();
        return;
      case "oom_kill":
        this.oom = {
          ...this.oom,
          [id]: { process: event.process, pid: event.pid, at: this.#now() },
        };
        return;
      case "workspace_progress": {
        const w = this.byId(id);
        const operation =
          w?.busy ??
          this.progress[id]?.operation ??
          operationOfStep(event.step);
        if (event.step === "done" || event.step === "failed") {
          const failed = event.step === "failed";
          this.#ended.set(id, (this.#ended.get(id) ?? 0) + 1);
          const { [id]: _gone, ...rest } = this.progress;
          this.progress = failed
            ? {
                ...rest,
                [id]: {
                  step: event.step,
                  detail: event.detail,
                  repository: event.repository ?? null,
                  operation,
                  failed,
                },
              }
            : rest;
          // `busy` is cleared before this event: the record now says what is left.
          void this.refresh();
          this.onSettled?.({
            id,
            label: w?.name ?? id,
            operation,
            failed,
            detail: event.detail,
          });
          return;
        }
        this.progress = {
          ...this.progress,
          [id]: {
            step: event.step,
            detail: event.detail,
            repository: event.repository ?? null,
            operation,
            failed: false,
          },
        };
        if (!w || w.busy === null) void this.refresh();
        return;
      }
    }
  }

  /** Forgets a failure the user has seen. */
  dismissProgress(id: string): void {
    const { [id]: _gone, ...rest } = this.progress;
    this.progress = rest;
  }

  /** A new operation hides the failure of the last one, but not steps of its own. */
  #forgetFailure(id: string): void {
    if (this.progress[id]?.failed) this.dismissProgress(id);
  }

  /**
   * Shows the record an action answered with, unless the operation already ended (events can
   * beat the answer): then the answer is older than what the page knows, so read again.
   * `startsOperation` also hides the last operation's failure, again only if this one has not ended.
   */
  #accept(
    id: string,
    before: number,
    record: Workspace,
    startsOperation = false,
  ): void {
    if ((this.#ended.get(id) ?? 0) === before) {
      // A failure that arrived before this answer belongs to this operation: keep it.
      if (startsOperation) this.#forgetFailure(id);
      this.#put(record);
    } else void this.refresh();
  }

  #endedCount(id: string): number {
    return this.#ended.get(id) ?? 0;
  }

  async #post<T>(
    run: () => Promise<{
      data?: T;
      error?: components["schemas"]["ApiErrorBody"];
      response: Response;
    }>,
  ): Promise<ActionResult<T>> {
    try {
      const { data, error, response } = await run();
      if (data !== undefined) return { ok: true, value: data };
      const message = error?.message ?? "Puddle refused that.";
      if (response.status === 404) {
        void this.refresh();
        return {
          ok: false,
          message: "That workspace no longer exists.",
          reason: "gone",
        };
      }
      if (response.status === 409) {
        void this.refresh();
        return { ok: false, message, reason: "conflict" };
      }
      return { ok: false, message, field: fieldOf(message) };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Creates a workspace; the clone runs on and reports through progress events. */
  async create(body: NewWorkspace): Promise<ActionResult<Workspace>> {
    // The id is not known until the answer: remember every count, then read the new one's.
    const counts = new Map(this.#ended);
    const result = await this.#post(() =>
      this.#api.POST("/api/workspaces", { body }),
    );
    if (result.ok) {
      this.#accept(
        result.value.id,
        counts.get(result.value.id) ?? 0,
        result.value,
        true,
      );
    } else if (!result.field && result.reason === "conflict") {
      return { ...result, field: "name" };
    }
    return result;
  }

  async #operate(
    id: string,
    op: "start" | "stop" | "reclaim" | "reset-root-disk",
  ): Promise<ActionResult<Workspace>> {
    const path = `/api/workspaces/{id}/${op}` as const;
    const before = this.#endedCount(id);
    const result = await this.#post(() =>
      this.#api.POST(path, { params: { path: { id } } }),
    );
    if (result.ok) {
      this.#accept(result.value.id, before, result.value, true);
    }
    return result;
  }

  startWorkspace = (id: string) => this.#operate(id, "start");
  stopWorkspace = (id: string) => this.#operate(id, "stop");
  reclaim = (id: string) => this.#operate(id, "reclaim");
  resetRootDisk = (id: string) => this.#operate(id, "reset-root-disk");

  /** Makes the root disk `sizeMib` big; it only grows, and the workspace is busy until it is done. */
  async growRootDisk(
    id: string,
    sizeMib: number,
  ): Promise<ActionResult<Workspace>> {
    const before = this.#endedCount(id);
    const result = await this.#post(() =>
      this.#api.POST("/api/workspaces/{id}/grow-root-disk", {
        params: { path: { id } },
        body: { size_mib: sizeMib },
      }),
    );
    if (result.ok) {
      this.#accept(result.value.id, before, result.value, true);
    }
    return result;
  }

  /** What deleting would lose. */
  checkDelete(id: string): Promise<ActionResult<DeleteCheck>> {
    return this.#post(() =>
      this.#api.GET("/api/workspaces/{id}/delete-check", {
        params: { path: { id } },
      }),
    );
  }

  /** Deletes the workspace the user saw `fingerprint` for. A 409 means it changed since. */
  async remove(
    id: string,
    fingerprint: string,
  ): Promise<ActionResult<Workspace>> {
    const before = this.#endedCount(id);
    const result = await this.#post(() =>
      this.#api.DELETE("/api/workspaces/{id}", {
        params: { path: { id } },
        body: { confirm: true, fingerprint },
      }),
    );
    if (result.ok) this.#accept(result.value.id, before, result.value);
    return result;
  }

  /**
   * Gives the workspace a new name. Only the name changes, so it works on a running workspace.
   * A 409 means another workspace has that name, or this one is being created or deleted.
   */
  async rename(id: string, name: string): Promise<ActionResult<Workspace>> {
    const result = await this.#post(() =>
      this.#api.PATCH("/api/workspaces/{id}", {
        params: { path: { id } },
        body: { name },
      }),
    );
    if (result.ok) this.#put(result.value);
    return result;
  }

  attach(id: string, mode: AttachMode): Promise<ActionResult<Attached>> {
    return this.#post(() =>
      this.#api.POST("/api/workspaces/{id}/attach", {
        params: { path: { id } },
        body: { mode },
      }),
    );
  }

  /**
   * Turns direct SSH on or off for one workspace by setting its own switch (the API replaces a
   * workspace's whole settings layer, so this reads it first and sends it back with the change).
   */
  async setDirectSsh(id: string, on: boolean): Promise<ActionResult> {
    try {
      const mine = await this.#api.GET("/api/settings/workspaces/{workspace}", {
        params: { path: { workspace: id } },
      });
      if (!mine.data) {
        return {
          ok: false,
          message: mine.error?.message ?? "Puddle couldn't read the settings.",
        };
      }
      const saved = await this.#api.PUT(
        "/api/settings/workspaces/{workspace}",
        {
          params: { path: { workspace: id } },
          body: { overrides: { ...mine.data.overrides, direct_ssh: on } },
        },
      );
      if (!saved.data) {
        return {
          ok: false,
          message: saved.error?.message ?? "Puddle couldn't save that.",
        };
      }
      await this.refresh();
      return { ok: true, value: null };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Starts listening and polling; returns the function that stops both. */
  start(): () => void {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unsubscribe = this.#source?.subscribe({
      event: (event) => this.handleEvent(event),
      resync: () => void this.refresh(),
    });
    const tick = () => {
      timer = setTimeout(() => {
        if (stopped) return;
        void this.refresh().finally(tick);
      }, this.#pollMs);
    };
    void this.refresh().finally(() => {
      if (!stopped) tick();
    });
    return () => {
      stopped = true;
      clearTimeout(timer);
      unsubscribe?.();
    };
  }
}

export const workspaces = new WorkspaceStore({ source: live });
