// SPDX-License-Identifier: GPL-3.0-or-later
/* eslint-disable svelte/prefer-svelte-reactivity -- the maps and sets here are bookkeeping that no template reads, so they need no reactivity */
// The inbox's state: open requests (grouped by registrable domain, R-18), the local toggles that
// decide whether a local destination can be approved (R-14), and the "decided just now" list with
// its undo.
//
// Live data: it listens to the shell's event stream and applies `pending_*` events in place. Until the API sends them it polls `/api/inbox`; the
// first pending event it sees turns the poll down to a slow safety refresh. Any resync
// (`lagged`, reconnect) refetches, and events that arrive during a refetch are applied after it,
// so the list always converges on the server's.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { asInboxEvent, type InboxEvent } from "#lib/decision/events.ts";
import { localCategory, type LocalCategory } from "#lib/decision/local.ts";
import type { Decided } from "#lib/decision/decided.ts";
import { build, type Choice, type Target } from "#lib/decision/model.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type PendingRequest = components["schemas"]["PendingRequest"];

export interface Row {
  request: PendingRequest;
  /** The group: registrable domain, or the IP literal. */
  domain: string;
}

export interface Group {
  domain: string;
  rows: Row[];
}

export type DecideResult =
  | { ok: true; decided: Decided }
  | { ok: false; reason: "stale" | "failed" | "invalid"; message: string };

export type UndoResult = { ok: true } | { ok: false; message: string };

export type Status = "loading" | "ready" | "failed";

/** The toggles of one workspace, as far as the inbox needs them. */
export type Toggles = Record<LocalCategory, boolean>;

type StoreApi = Pick<ApiClient, "GET" | "POST" | "DELETE">;

export interface PendingDeps {
  api?: StoreApi;
  source?: LiveSource;
  now?: () => number;
  /** Poll while the API sends no pending events. */
  pollMs?: number;
  /** Poll once pending events have been seen. */
  slowPollMs?: number;
  /** Told the open-request count after every change (the nav badge). */
  onCount?: (count: number) => void;
}

const MAX_DECIDED = 8;

/** A performance mark for the e2e speed bar: when the first list reached the page. */
function markOnce(name: string): void {
  if (
    typeof performance !== "undefined" &&
    performance.getEntriesByName(name).length === 0
  ) {
    performance.mark(name);
  }
}

export function groupRows(rows: readonly Row[]): Group[] {
  const byDomain = new Map<string, Row[]>();
  for (const row of rows) {
    const list = byDomain.get(row.domain);
    if (list) list.push(row);
    else byDomain.set(row.domain, [row]);
  }
  const newest = (g: Row[]) =>
    Math.max(...g.map((r) => r.request.first_seen), 0);
  return [...byDomain.entries()]
    .map(([domain, list]) => ({
      domain,
      rows: [...list].sort(
        (a, b) =>
          b.request.first_seen - a.request.first_seen ||
          b.request.id - a.request.id,
      ),
    }))
    .sort((a, b) => newest(b.rows) - newest(a.rows));
}

export interface ShownGroup extends Group {
  /** All the group's rows, of which `rows` shows the first ones. */
  total: number;
}

/**
 * How many rows a long inbox draws first, and how many more a frame later. The numbers are a
 * paint-speed choice; a unit test shrinks them so the slicing is checked on a handful of rows
 * instead of the cost of drawing sixty.
 */
export const slices = { first: 60, next: 250 };

/** The first `limit` rows of the groups, in order; groups with no row left are dropped. */
export function limitGroups(
  groups: readonly Group[],
  limit: number,
): ShownGroup[] {
  const shown: ShownGroup[] = [];
  let left = limit;
  for (const group of groups) {
    if (left <= 0) break;
    shown.push({
      ...group,
      rows: group.rows.slice(0, left),
      total: group.rows.length,
    });
    left -= group.rows.length;
  }
  return shown;
}

export class PendingStore {
  rows = $state.raw<Row[]>([]);
  /** The most open requests the service lists per workspace (R-45); a workspace at it has a full list. */
  openLimit = $state(Number.POSITIVE_INFINITY);
  /** `null` while unknown; a workspace whose settings could not be read is treated as approvable. */
  toggles = $state.raw<Record<string, Toggles | null>>({});
  decided = $state.raw<Decided[]>([]);
  status = $state<Status>("loading");
  /** Why the last read of the inbox failed; `null` once a read works. */
  loadError = $state<string | null>(null);

  readonly groups = $derived(groupRows(this.rows));
  readonly count = $derived(this.rows.length);
  /** The workspaces whose list is full: a new host they ask about is not listed until some are answered. */
  readonly full = $derived.by(() => {
    const open = new Map<string, number>();
    for (const { request } of this.rows) {
      open.set(request.workspace, (open.get(request.workspace) ?? 0) + 1);
    }
    return [...open]
      .filter(([, n]) => n >= this.openLimit)
      .map(([workspace]) => workspace)
      .sort();
  });

  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;
  readonly #now: () => number;
  readonly #pollMs: number;
  readonly #slowPollMs: number;
  readonly #onCount: ((count: number) => void) | undefined;
  #buffer: InboxEvent[] | null = null;
  #sawPendingEvents = false;
  #refreshing: Promise<void> | null = null;
  #again = false;
  #refetchTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(deps: PendingDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#source = deps.source;
    this.#now = deps.now ?? Date.now;
    this.#pollMs = deps.pollMs ?? 5000;
    this.#slowPollMs = deps.slowPollMs ?? 60_000;
    this.#onCount = deps.onCount;
  }

  /** The workspaces that have an open request. */
  get workspaces(): string[] {
    return [...new Set(this.rows.map((r) => r.request.workspace))].sort();
  }

  /** Why a request can't be approved yet: the toggle it needs is off (R-14). */
  blockedBy(request: PendingRequest): LocalCategory | null {
    const category = localCategory(request.host);
    if (category === null) return null;
    const toggles = this.toggles[request.workspace];
    return toggles && !toggles[category] ? category : null;
  }

  /** Reads everything the screen shows; never throws. Concurrent calls share one run. */
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
      const { data, error, response } = await this.#api.GET("/api/inbox");
      if (!data) {
        this.loadError = readFailure(error, response);
        if (this.status === "loading") this.status = "failed";
        return;
      }
      this.loadError = null;
      this.openLimit = data.open_limit;
      const rows: Row[] = data.groups.flatMap((g) =>
        g.requests.map((request) => ({
          request,
          domain: g.registrable_domain,
        })),
      );
      if (this.status !== "ready") markOnce("puddle:inbox-data");
      this.#setRows(rows);
      this.status = "ready";
      await this.#loadToggles();
    } catch {
      this.loadError = SERVICE_DOWN;
      if (this.status === "loading") this.status = "failed";
    }
  }

  async #loadToggles(): Promise<void> {
    const wanted = new Set(
      this.rows
        .filter((r) => localCategory(r.request.host) !== null)
        .map((r) => r.request.workspace),
    );
    const next: Record<string, Toggles | null> = {};
    await Promise.all(
      [...wanted].map(async (workspace) => {
        try {
          const { data } = await this.#api.GET(
            "/api/settings/workspaces/{workspace}",
            { params: { path: { workspace } } },
          );
          const t = data?.effective.local_toggles;
          next[workspace] = t
            ? {
                loopback: t.loopback.value,
                private: t.private.value,
                link_local: t.link_local.value,
                metadata: t.metadata.value,
                special: t.special.value,
              }
            : null;
        } catch {
          next[workspace] = null;
        }
      }),
    );
    this.toggles = next;
  }

  #setRows(rows: Row[]): void {
    this.rows = rows;
    this.#onCount?.(rows.length);
  }

  /** Applies one stream event in place. Anything that isn't an inbox event is ignored. */
  handleEvent(raw: unknown): void {
    const event = asInboxEvent(raw);
    if (!event) return;
    this.#sawPendingEvents = true;
    if (this.#buffer) this.#buffer.push(event);
    else this.#apply(event);
  }

  #apply(event: InboxEvent): void {
    switch (event.type) {
      case "pending_opened": {
        const { request } = event;
        if (this.rows.some((r) => r.request.id === request.id)) return;
        const domain = event.registrable_domain;
        if (!domain) {
          // The group is the server's to say (public suffix list): fetch instead of guessing.
          this.#refetchSoon();
          return;
        }
        this.#setRows([{ request, domain }, ...this.rows]);
        if (localCategory(request.host) !== null) void this.#loadToggles();
        return;
      }
      case "pending_updated":
        this.#setRows(
          this.rows.map((r) =>
            r.request.id === event.id
              ? {
                  ...r,
                  request: {
                    ...r.request,
                    attempts: event.attempts,
                    last_seen: event.last_seen,
                  },
                }
              : r,
          ),
        );
        return;
      case "pending_closed":
        this.#setRows(this.rows.filter((r) => r.request.id !== event.id));
        return;
    }
  }

  #refetchSoon(): void {
    clearTimeout(this.#refetchTimer);
    this.#refetchTimer = setTimeout(() => void this.refresh(), 150);
  }

  /** Approves or denies a request. A global choice needs `confirmed`. */
  async decide(
    row: Row,
    choice: Choice,
    confirmed: boolean,
  ): Promise<DecideResult> {
    const target: Target = {
      host: row.request.host,
      registrableDomain: row.domain,
    };
    const built = build(choice, target, confirmed);
    if (!built.ok) {
      return { ok: false, reason: "invalid", message: built.error };
    }
    const path = {
      params: { path: { id: row.request.id } },
      body: built.body,
    };
    try {
      const { data, error, response } =
        built.effect === "allow"
          ? await this.#api.POST("/api/pending/{id}/approve", path)
          : await this.#api.POST("/api/pending/{id}/deny", path);
      if (!data) {
        if (response.status === 409 || response.status === 404) {
          void this.refresh();
          return {
            ok: false,
            reason: "stale",
            message: "This request was already decided or has gone away.",
          };
        }
        return {
          ok: false,
          reason: "failed",
          message: error?.message ?? "puddle's service refused the decision.",
        };
      }
      const closed = new Set<number>([data.request.id, ...data.also_closed]);
      this.#setRows(this.rows.filter((r) => !closed.has(r.request.id)));
      const rule = data.rule;
      const decided: Decided = {
        ruleId: rule.id,
        effect: rule.effect,
        pattern: rule.pattern,
        patternKind: rule.pattern_kind,
        workspace:
          rule.scope.type === "workspace" ? rule.scope.workspace : null,
        ruleSet: choice.ruleSet?.name ?? null,
        expiresAt: rule.expires_at,
        alsoClosed: data.also_closed.length,
        at: this.#now(),
      };
      this.decided = [decided, ...this.decided].slice(0, MAX_DECIDED);
      return { ok: true, decided };
    } catch {
      return {
        ok: false,
        reason: "failed",
        message: "puddle's service isn't answering.",
      };
    }
  }

  /** Deletes the rule a decision created. The request returns when the workspace retries. */
  async undo(decided: Decided): Promise<UndoResult> {
    try {
      const { response } = await this.#api.DELETE("/api/rules/{id}", {
        params: { path: { id: decided.ruleId } },
      });
      // 404: someone else already deleted it, which is what the user wanted.
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "puddle couldn't undo that." };
      }
      this.decided = this.decided.filter((d) => d.ruleId !== decided.ruleId);
      return { ok: true };
    } catch {
      return { ok: false, message: "puddle's service isn't answering." };
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
      timer = setTimeout(
        () => {
          if (stopped) return;
          void this.refresh().finally(tick);
        },
        this.#sawPendingEvents ? this.#slowPollMs : this.#pollMs,
      );
    };
    void this.refresh().finally(() => {
      if (!stopped) tick();
    });
    return () => {
      stopped = true;
      clearTimeout(timer);
      clearTimeout(this.#refetchTimer);
      unsubscribe?.();
    };
  }
}

export const pending = new PendingStore({
  source: live,
  onCount: (count) => live.setPending(count),
});
