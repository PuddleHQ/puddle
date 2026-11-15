// SPDX-License-Identifier: AGPL-3.0-or-later
// The one-time notice for an install that was set up before the interception choice existed: it
// keeps decrypting only the hosts Puddle has a reason to read until the user picks a mode. The
// notice stays until it is answered: "Keep it as it is" says so, choosing in Settings does too.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { isWelcome } from "#lib/welcome/steps.ts";
import type { Notifier } from "./notifier.ts";

export const INTERCEPTION_NOTICE_KEY = "interception-choice";

export class InterceptionChoiceNotice {
  readonly #notifier: Notifier;
  readonly #api: Pick<ApiClient, "GET" | "PUT">;

  readonly #path: () => string;

  constructor(
    notifier: Notifier,
    api: Pick<ApiClient, "GET" | "PUT"> = defaultApi,
    path: () => string = () => globalThis.location?.pathname ?? "",
  ) {
    this.#notifier = notifier;
    this.#api = api;
    this.#path = path;
  }

  /**
   * Raises the notice while the choice is open and unanswered; never throws. Not during the
   * first-run flow, which asks the question itself.
   */
  async refresh(): Promise<void> {
    if (isWelcome(this.#path())) return;
    try {
      const { data } = await this.#api.GET("/api/interception");
      if (data?.chosen !== false || data.notice_seen !== false) return;
    } catch {
      return;
    }
    this.#notifier.notify({
      key: INTERCEPTION_NOTICE_KEY,
      tone: "info",
      title:
        "New: you can let Puddle decrypt every HTTPS host, not only the ones it has a reason to read.",
      detail:
        "Today it decrypts only those, which limits what a rule on another host can do. Nothing changes until you choose.",
      link: { href: "/settings#s-net", label: "Choose in Settings" },
      action: { label: "Keep it as it is", run: () => this.#keep() },
    });
  }

  async #keep(): Promise<void> {
    const { data } = await this.#api.PUT("/api/interception", {
      body: { notice_seen: true },
    });
    if (!data) throw new Error("Puddle couldn't keep that choice.");
    this.#notifier.resolve(INTERCEPTION_NOTICE_KEY);
  }
}
