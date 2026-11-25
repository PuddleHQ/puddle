// SPDX-License-Identifier: AGPL-3.0-or-later
// The owner's admin screen: the accounts, the server's totals and the audit. The invite secret a
// create or a reissue answers with is handed to the caller once and never kept here.
import type {
  AdminStats,
  AdminUser,
  AuditLine,
  InviteIssued,
} from "#lib/admin/model.ts";
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { sentence } from "#lib/rules/model.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type Status = "loading" | "ready" | "failed";
export type Issued =
  { ok: true; invite: InviteIssued } | { ok: false; message: string };
export type Result = { ok: true } | { ok: false; message: string };
type StoreApi = Pick<ApiClient, "GET" | "POST" | "DELETE">;

const NONE_USERS: AdminUser[] = [];
const NONE_LINES: AuditLine[] = [];

function refusal(error: { message?: string } | undefined): string {
  return error?.message ? sentence(error.message) : "Puddle refused that.";
}

export class AdminStore {
  /** Whether this Puddle has an admin screen for the viewer: several users on and the owner. */
  available = $state(false);
  /** The last read found no admin screen for this viewer (a Puddle with one user). */
  missing = $state(false);
  status = $state<Status>("loading");
  users = $state.raw<AdminUser[]>(NONE_USERS);
  stats = $state.raw<AdminStats | null>(null);
  audit = $state.raw<AuditLine[]>(NONE_LINES);
  reason = $state<string | null>(null);
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  /** Finds out whether the screen exists for this viewer; the server answers 404 (403 for a visitor) when it does not. */
  async probe(): Promise<void> {
    try {
      const { response } = await this.#api.GET("/api/admin/stats");
      // Only the server saying "no such screen for you" hides it; a 5xx, an expired token or a
      // dropped connection leaves what was known.
      if (response.ok) this.available = true;
      else if (response.status === 403 || response.status === 404) {
        this.available = false;
      }
    } catch {
      // Not answering says nothing about the screen.
    }
  }

  async load(): Promise<void> {
    const ticket = ++this.#ticket;
    this.reason = null;
    try {
      // The audit is read after the users: the server writes a finished deletion's audit line in the
      // same transaction that removes the account, so an audit read that starts once the account
      // shows as gone always has the line (a parallel read could miss it, and nothing polls again).
      const [users, stats] = await Promise.all([
        this.#api.GET("/api/admin/users"),
        this.#api.GET("/api/admin/stats"),
      ]);
      const audit = await this.#api.GET("/api/admin/audit");
      // A newer read started meanwhile: its answer is the one to show.
      if (ticket !== this.#ticket) return;
      if (users.data && stats.data && audit.data) {
        this.users = users.data.users;
        this.stats = stats.data;
        this.audit = audit.data.lines;
        this.available = true;
        this.missing = false;
        this.status = "ready";
        return;
      }
      const failed = !users.data ? users : !stats.data ? stats : audit;
      const missing = failed.response.status === 404;
      this.missing = missing;
      if (missing) this.available = false;
      this.status = "failed";
      this.reason = missing
        ? "There is no admin screen here: it is for the owner of a Puddle with several users."
        : readFailure(failed.error, failed.response);
    } catch {
      if (ticket !== this.#ticket) return;
      this.status = "failed";
      this.reason = SERVICE_DOWN;
    }
  }

  /** Makes an account; the invite comes back once, for the caller to show. */
  async create(name: string): Promise<Issued> {
    try {
      const { data, error } = await this.#api.POST("/api/admin/users", {
        body: { name },
      });
      if (!data) return { ok: false, message: refusal(error) };
      await this.load();
      return { ok: true, invite: data };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** A new invite for an account (for an active one this signs it out and ends its password). */
  async reissue(id: number): Promise<Issued> {
    try {
      const { data, error } = await this.#api.POST(
        "/api/admin/users/{id}/invite",
        { params: { path: { id } } },
      );
      if (!data) return { ok: false, message: refusal(error) };
      await this.load();
      return { ok: true, invite: data };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }

  /** Starts deleting an account; the server finishes it, so the list shows it as being deleted. */
  async remove(id: number, confirm: string): Promise<Result> {
    try {
      const { error, response } = await this.#api.DELETE(
        "/api/admin/users/{id}",
        { params: { path: { id }, query: { confirm } } },
      );
      // An account that is gone already is as good as deleted.
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: refusal(error) };
      }
      await this.load();
      return { ok: true };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    }
  }
}

export const admin = new AdminStore();
