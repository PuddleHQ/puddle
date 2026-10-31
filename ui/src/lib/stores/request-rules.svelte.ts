// SPDX-License-Identifier: GPL-3.0-or-later
// The rules on method and path (`/api/request-rules`) and the two things a user does to one: add
// it and delete it. It refetches on `request_rules_changed` and on a resync, and polls slowly as a
// safety net for a dropped event stream.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { sentence } from "#lib/rules/model.ts";
import { isRequestRulesChanged } from "#lib/rules/request-rule-events.ts";
import {
  refusalField,
  type NewRequestRule,
  type RequestField,
  type RequestRule,
} from "#lib/rules/request-rules.ts";
import { live, type LiveSource } from "./live.svelte.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type Status = "loading" | "ready" | "failed";

/** A refusal and the form field it belongs to. */
export interface RequestRuleRefusal {
  field: RequestField;
  message: string;
}

export type AddResult =
  { ok: true; rule: RequestRule } | ({ ok: false } & RequestRuleRefusal);
export type RemoveResult = { ok: true } | { ok: false; message: string };

type StoreApi = Pick<ApiClient, "GET" | "POST" | "DELETE">;

export interface RequestRulesDeps {
  api?: StoreApi;
  source?: LiveSource;
  pollMs?: number;
}

export class RequestRulesStore {
  rules = $state.raw<RequestRule[]>([]);
  status = $state<Status>("loading");
  /** Why the last read failed; `null` once a read works. */
  loadError = $state<string | null>(null);

  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;
  readonly #pollMs: number;

  constructor(deps: RequestRulesDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#source = deps.source;
    this.#pollMs = deps.pollMs ?? 60_000;
  }

  /** Reads every rule on method and path; never throws. */
  async refresh(): Promise<void> {
    try {
      const { data, error, response } =
        await this.#api.GET("/api/request-rules");
      if (data) {
        this.rules = data.rules;
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

  /** Makes a rule. The API's refusals (host, methods, path, the limit) come back as text. */
  async add(rule: NewRequestRule): Promise<AddResult> {
    try {
      const { data, error } = await this.#api.POST("/api/request-rules", {
        body: rule,
      });
      if (!data) {
        const refusal = refusalField(
          error?.message ?? "Puddle refused the rule",
        );
        return {
          ok: false,
          field: refusal.field,
          message: sentence(refusal.message),
        };
      }
      this.rules = [...this.rules.filter((r) => r.id !== data.id), data];
      return { ok: true, rule: data };
    } catch {
      return { ok: false, field: "form", message: SERVICE_DOWN };
    }
  }

  /** Deletes a rule. A rule that is already gone counts as deleted. */
  async remove(id: number): Promise<RemoveResult> {
    try {
      const { response } = await this.#api.DELETE("/api/request-rules/{id}", {
        params: { path: { id } },
      });
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't delete that rule." };
      }
      this.rules = this.rules.filter((r) => r.id !== id);
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** Starts listening and polling; returns the function that stops both. */
  start(): () => void {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (isRequestRulesChanged(event)) void this.refresh();
      },
      resync: () => void this.refresh(),
    });
    const tick = () => {
      if (stopped) return;
      timer = setTimeout(() => {
        void this.refresh().finally(tick);
      }, this.#pollMs);
    };
    void this.refresh().finally(tick);
    return () => {
      stopped = true;
      clearTimeout(timer);
      unsubscribe?.();
    };
  }
}

export const requestRulesStore = new RequestRulesStore({ source: live });
