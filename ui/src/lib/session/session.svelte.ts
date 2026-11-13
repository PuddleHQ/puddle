// SPDX-License-Identifier: AGPL-3.0-or-later
// How this browser is signed in. The desktop window has the shell's token and never signs in
// (mode `local`). A browser that reached Puddle under a public name signs in with a name and a
// password; the service sets a cookie the page cannot read and answers with a 15-minute access
// token, which lives here in memory only and is replaced shortly before it runs out. Nothing about
// the session is written to storage, a URL or the page's own cookies.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { bootstrapToken, useSessionToken } from "#lib/api/connection.ts";
import type { components } from "#lib/api/schema.d.ts";
import { sentence } from "#lib/rules/model.ts";
import { deviceLabel } from "./device-label.ts";

export type SessionGrant = components["schemas"]["SessionGrant"];
export type UserView = components["schemas"]["UserView"];

/**
 * `checking`: asking the service. `local`: the desktop window (or a service with no accounts),
 * nothing to sign in to. `setup`: no owner exists yet, so the first visitor makes it.
 * `signed-out`: a name and a password are asked for. `signed-in`: requests carry the access token.
 * `unavailable`: the service did not answer.
 */
export type Mode =
  "checking" | "local" | "setup" | "signed-out" | "signed-in" | "unavailable";

export type Outcome = { ok: true } | { ok: false; message: string };

type SessionApi = Pick<ApiClient, "GET" | "POST">;

/** The fewest characters a password has (the service checks too). */
export const MIN_PASSWORD_CHARS = 12;
/** A new access token is fetched this long before the old one runs out. */
export const REFRESH_EARLY_MS = 60_000;
/** After a refresh that could not reach the service, the next try comes this much later. */
export const RETRY_MS = 15_000;
/** The soonest a refresh is scheduled, so a clock that is off does not spin. */
const SOONEST_MS = 5_000;

export interface SessionDeps {
  api?: SessionApi;
  /** The shell's token, if any (the desktop window). */
  bootstrap?: () => string | undefined;
  now?: () => number;
  setTimer?: (run: () => void, ms: number) => unknown;
  clearTimer?: (timer: unknown) => void;
  userAgent?: () => string;
  /** The page's path and fragment at start, to find an invite link. */
  location?: () => { pathname: string; hash: string };
  /** Removes the fragment from the address bar, so a secret does not stay in history. */
  dropFragment?: () => void;
}

const SERVICE_DOWN = "Puddle's service isn't answering.";

export class SessionStore {
  mode = $state<Mode>("checking");
  user = $state.raw<UserView | null>(null);
  device = $state.raw<SessionGrant["device"] | null>(null);
  /** Whether a password signs in (the owner can switch it off). */
  passwordLogin = $state(true);
  /** The secret of an invite link the page was opened with, until it is used. */
  invite = $state<string | null>(null);
  busy = $state(false);

  #token: string | undefined;
  #timer: unknown = null;
  #recovering: Promise<boolean> | null = null;
  readonly #api: SessionApi;
  readonly #bootstrap: () => string | undefined;
  readonly #now: () => number;
  readonly #setTimer: (run: () => void, ms: number) => unknown;
  readonly #clearTimer: (timer: unknown) => void;
  readonly #userAgent: () => string;
  readonly #location: () => { pathname: string; hash: string };
  readonly #dropFragment: () => void;

  constructor(deps: SessionDeps = {}) {
    this.#api = deps.api ?? defaultApi;
    this.#bootstrap = deps.bootstrap ?? bootstrapToken;
    this.#now = deps.now ?? (() => Date.now());
    this.#setTimer = deps.setTimer ?? ((run, ms) => setTimeout(run, ms));
    this.#clearTimer =
      deps.clearTimer ??
      ((timer) => clearTimeout(timer as ReturnType<typeof setTimeout>));
    this.#userAgent =
      deps.userAgent ?? (() => globalThis.navigator?.userAgent ?? "");
    this.#location =
      deps.location ??
      (() => ({
        pathname: globalThis.location?.pathname ?? "",
        hash: globalThis.location?.hash ?? "",
      }));
    this.#dropFragment =
      deps.dropFragment ??
      (() =>
        globalThis.history?.replaceState(
          null,
          "",
          globalThis.location.pathname + globalThis.location.search,
        ));
  }

  /** The access token requests carry, while signed in. */
  token(): string | undefined {
    return this.#token;
  }

  /** Whether the app's screens may load: signed in, or nothing to sign in to. */
  get ready(): boolean {
    return this.mode === "signed-in" || this.mode === "local";
  }

  /** Finds out how this page is signed in; never throws. */
  async start(): Promise<void> {
    useSessionToken(() => this.#token);
    if (this.#bootstrap() !== undefined) {
      this.mode = "local";
      return;
    }
    this.#takeInvite();
    try {
      const { data, response } = await this.#api.GET("/api/auth/status");
      if (!data) {
        // No sign-in under this address: the loopback address wants the install token, which a
        // development proxy adds itself.
        this.mode = response.status === 503 ? "unavailable" : "local";
        return;
      }
      this.passwordLogin = data.password_login;
      if (!data.setup_open && (await this.#refresh()) === "signed-in") return;
      this.mode = data.setup_open ? "setup" : "signed-out";
    } catch {
      this.mode = "unavailable";
    }
  }

  /** Makes the owner account (the first visitor), and signs in as it. */
  async setup(name: string, password: string): Promise<Outcome> {
    return this.#grantFrom(
      () =>
        this.#api.POST("/api/auth/setup", {
          body: { name, password, device_name: this.#label() },
        }),
      async () => {
        // Somebody else was first: what is left is to sign in.
        await this.start();
      },
    );
  }

  /** Signs in with a name and a password. */
  async signIn(name: string, password: string): Promise<Outcome> {
    return this.#grantFrom(() =>
      this.#api.POST("/api/auth/sign-in", {
        body: { name, password, device_name: this.#label() },
      }),
    );
  }

  /** Sets a password with the secret of an invite link, and signs in. */
  async redeemInvite(secret: string, password: string): Promise<Outcome> {
    const outcome = await this.#grantFrom(() =>
      this.#api.POST("/api/auth/invite", {
        body: { secret, password, device_name: this.#label() },
      }),
    );
    if (outcome.ok) this.invite = null;
    return outcome;
  }

  /** Signs this browser out: the service revokes its device and clears the cookie. */
  async signOut(): Promise<void> {
    try {
      await this.#api.POST("/api/auth/sign-out");
    } catch {
      // The cookie may stay if the service did not answer; the access token goes either way.
    }
    this.#forget();
    this.mode = "signed-out";
  }

  /**
   * A request was refused as unauthorised: the access token ran out early or the device was
   * revoked. Asks for a new token once (several callers share one attempt) and says whether the
   * browser is still signed in.
   */
  recover(): Promise<boolean> {
    if (this.mode !== "signed-in") return Promise.resolve(false);
    this.#recovering ??= this.#refresh().then((outcome) => {
      if (outcome === "refused" && this.mode === "signed-in") {
        this.#forget();
        this.mode = "signed-out";
      }
      this.#recovering = null;
      return outcome === "signed-in";
    });
    return this.#recovering;
  }

  /** Stops the refresh timer. */
  stop(): void {
    this.#cancelTimer();
  }

  #label(): string {
    return deviceLabel(this.#userAgent());
  }

  #takeInvite(): void {
    const { pathname, hash } = this.#location();
    if (pathname !== "/invite" || hash.length < 2) return;
    this.invite = decodeURIComponent(hash.slice(1));
    // The secret is in the page's memory now; it should not stay in the address bar.
    this.#dropFragment();
  }

  async #grantFrom(
    request: () => Promise<{
      data?: SessionGrant;
      error?: { message?: string };
      response: Response;
    }>,
    onConflict?: () => Promise<void>,
  ): Promise<Outcome> {
    if (this.busy) return { ok: false, message: "One moment." };
    this.busy = true;
    try {
      const { data, error, response } = await request();
      if (data) {
        this.#accept(data);
        return { ok: true };
      }
      if (response.status === 409 && onConflict) await onConflict();
      return { ok: false, message: failure(error?.message) };
    } catch {
      return { ok: false, message: SERVICE_DOWN };
    } finally {
      this.busy = false;
    }
  }

  /**
   * Asks for a new access token with the cookie. `refused`: the cookie is no session (any more).
   * `unreachable`: the service did not give an answer, which says nothing about the session, so
   * the browser keeps what it has and asks again soon.
   */
  async #refresh(): Promise<"signed-in" | "refused" | "unreachable"> {
    try {
      const { data, response } = await this.#api.POST("/api/auth/refresh");
      if (data) {
        this.#accept(data);
        return "signed-in";
      }
      if (response.status === 401) return "refused";
      this.#later(RETRY_MS);
      return "unreachable";
    } catch {
      this.#later(RETRY_MS);
      return "unreachable";
    }
  }

  #accept(grant: SessionGrant): void {
    this.#token = grant.access_token;
    this.user = grant.user;
    this.device = grant.device;
    this.mode = "signed-in";
    this.#later(
      Math.max(SOONEST_MS, grant.expires_at - REFRESH_EARLY_MS - this.#now()),
    );
  }

  #later(ms: number): void {
    this.#cancelTimer();
    this.#timer = this.#setTimer(() => {
      this.#timer = null;
      if (this.mode === "signed-in") void this.recover();
    }, ms);
  }

  #cancelTimer(): void {
    if (this.#timer !== null) this.#clearTimer(this.#timer);
    this.#timer = null;
  }

  #forget(): void {
    this.#cancelTimer();
    this.#token = undefined;
    this.user = null;
    this.device = null;
  }
}

/** What a refusal says in the sign-in form: the service's own words, which never echo a secret. */
function failure(message: string | undefined): string {
  return message ? sentence(message) : "Puddle refused that.";
}

export const session = new SessionStore();
