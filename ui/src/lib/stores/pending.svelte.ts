// SPDX-License-Identifier: AGPL-3.0-or-later
/* eslint-disable svelte/prefer-svelte-reactivity -- the maps and sets here are bookkeeping that no template reads, so they need no reactivity */
// The inbox's state: open requests (grouped by registrable domain, R-18), the local destinations whose
// toggle is off (R-14, named by the service), and the "decided just now" list with
// its undo.
//
// Live data: it listens to the shell's event stream and applies `pending_*` events in place. Until the API sends them it polls `/api/inbox`; the
// first pending event it sees turns the poll down to a slow safety refresh. Any resync
// (`lagged`, reconnect) refetches, and events that arrive during a refetch are applied after it,
// so the list always converges on the server's.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { asInboxEvent, type InboxEvent } from "#lib/decision/events.ts";
import type { LocalCategory } from "#lib/decision/local-labels.ts";
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

type StoreApi = Pick<ApiClient, "GET" | "POST">;

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
  /** Connections a rule allowed since the start of the UTC day; the service counts in memory, so a restart resets it. */
  allowedToday = $state(0);
  decided = $state.raw<Decided[]>([]);
  status = $state<Status>("loading");
  /** Why the last read of the inbox failed; `null` once a read works. */
  loadError = $state<string | null>(null);

  /**
   * The ids the page may draw. A request that arrives while the page is open is held back until
   * the user asks for it (`showHeld`): the guest picks the moment it arrives, so a row must never
   * move under a pointer that is about to click Allow (an Allow click on the wrong request).
   */
  #shown = $state.raw<ReadonlySet<number>>(new Set());
  /** The rows the user has been shown: the ones the page draws. */
  readonly visible = $derived(
    this.rows.filter((r) => this.#shown.has(r.request.id)),
  );
  /** How many open requests arrived since and wait behind the "new requests" line. */
  readonly held = $derived(this.rows.length - this.visible.length);
  readonly groups = $derived(groupRows(this.visible));
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
  /** Requests decided here since the running read began; the read may predate the decision. */
  #decidedMeanwhile: number[] = [];
  #refetchTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(deps: PendingDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#source = deps.source;
    this.#now = deps.now ?? Date.now;
    this.#pollMs = deps.pollMs ?? 5000;
    this.#slowPollMs = deps.slowPollMs ?? 60_000;
    this.#onCount = deps.onCount;
  }

  /** Draws the requests held back since the user last looked. */
  showHeld(): void {
    this.#setRows(this.rows, "all");
  }

  /** The workspaces that have an open request. */
  get workspaces(): string[] {
    return [...new Set(this.rows.map((r) => r.request.workspace))].sort();
  }

  /** Why a request can't be approved yet: the toggle it needs is off (R-14). The service says so. */
  blockedBy(request: PendingRequest): LocalCategory | null {
    return request.blocked_by;
  }

  /** Reads everything the screen shows; never throws. Concurrent calls share one run. */
  refresh(): Promise<void> {
    if (this.#refreshing) {
      this.#again = true;
      return this.#refreshing;
    }
    this.#buffer = [];
    this.#decidedMeanwhile = [];
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
      this.allowedToday = data.allowed_today;
      const rows: Row[] = data.groups.flatMap((g) =>
        g.requests
          .filter((request) => !this.#decidedMeanwhile.includes(request.id))
          .map((request) => ({ request, domain: g.registrable_domain })),
      );
      const first = this.status !== "ready";
      if (first) markOnce("puddle:inbox-data");
      this.#setRows(rows, first ? "all" : []);
      this.status = "ready";
    } catch {
      this.loadError = SERVICE_DOWN;
      if (this.status === "loading") this.status = "failed";
    }
  }

  /**
   * Replaces the rows. Rows the user has not been shown stay held unless named in `reveal` (or
   * `"all"`); with nothing drawn there is nothing to move, so everything is drawn.
   */
  #setRows(rows: Row[], reveal: readonly number[] | "all" = []): void {
    const drawn = rows.filter((r) => this.#shown.has(r.request.id));
    const ids =
      reveal === "all" || drawn.length === 0
        ? rows
        : [...drawn, ...rows.filter((r) => reveal.includes(r.request.id))];
    this.#shown = new Set(ids.map((r) => r.request.id));
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
        // The short form of an event doesn't say which toggle blocks it; the service does.
        this.#refetchSoon();
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
          message: error?.message ?? "Puddle's service refused the decision.",
        };
      }
      const closed = new Set<number>([data.request.id, ...data.also_closed]);
      const closedRequests = this.rows
        .filter((r) => closed.has(r.request.id))
        .map(({ request, domain }) => ({
          workspace: request.workspace,
          host: request.host,
          port: request.port,
          domain,
        }));
      this.#decidedMeanwhile.push(...closed);
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
        closed: closedRequests,
        at: this.#now(),
      };
      this.decided = [decided, ...this.decided].slice(0, MAX_DECIDED);
      return { ok: true, decided };
    } catch {
      return {
        ok: false,
        reason: "failed",
        message: "Puddle's service isn't answering.",
      };
    }
  }

  /**
   * Takes a decision back: the service deletes the rule and opens the requests it closed again
   * (new rows with the old first-seen times, so each sorts where it did). A rule already gone
   * counts as undone; there is nothing left to put back.
   */
  async undo(decided: Decided): Promise<UndoResult> {
    try {
      const { data, response } = await this.#api.POST("/api/rules/{id}/undo", {
        params: { path: { id: decided.ruleId } },
      });
      if (!data && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't undo that." };
      }
      this.decided = this.decided.filter((d) => d.ruleId !== decided.ruleId);
      if (data) this.#reopen(data.reopened, decided.closed);
      return { ok: true };
    } catch {
      return { ok: false, message: "Puddle's service isn't answering." };
    }
  }

  /** Lists reopened requests again, each in the group it sat in; the stream's event for it finds it there. */
  #reopen(reopened: PendingRequest[], closed: Decided["closed"]): void {
    const domains = new Map(
      closed.map((c) => [`${c.workspace}\0${c.host}\0${c.port}`, c.domain]),
    );
    const back: Row[] = [];
    let unknown = false;
    for (const request of reopened) {
      if (this.rows.some((r) => r.request.id === request.id)) continue;
      const domain = domains.get(
        `${request.workspace}\0${request.host}\0${request.port}`,
      );
      if (domain === undefined) unknown = true;
      else back.push({ request, domain });
    }
    if (back.length > 0) {
      this.#setRows(
        [...back, ...this.rows],
        back.map((r) => r.request.id),
      );
    }
    // The service names the group; ask it when a request came back that this page did not decide.
    if (unknown) void this.refresh();
  }

  /**
   * Starts listening and polling; returns the function that stops both. The shell's strip and the
   * Inbox both start it: one listener and one poll run while any caller holds it.
   */
  start(): () => void {
    this.#holders += 1;
    if (this.#holders === 1) this.#stopRun = this.#run();
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.#holders -= 1;
      if (this.#holders === 0) this.#stopRun?.();
    };
  }

  #holders = 0;
  #stopRun: (() => void) | undefined;

  #run(): () => void {
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
