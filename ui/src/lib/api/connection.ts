// SPDX-License-Identifier: AGPL-3.0-or-later
// How the page learns the API token. The desktop shell sets `window.__PUDDLE__` with an init
// script before the app starts; in `npm run dev` the Vite proxy adds the token to /api requests
// itself, so the page never holds one. The token is never read from a URL, a cookie or storage,
// and never written to any of them.

/** What the shell hands the page. */
export interface PuddleBootstrap {
  /** The API bearer token. */
  token?: string;
}

/** The token, if the shell provided one. */
export function bootstrapToken(
  scope: { __PUDDLE__?: PuddleBootstrap } = globalThis as {
    __PUDDLE__?: PuddleBootstrap;
  },
): string | undefined {
  const token = scope.__PUDDLE__?.token;
  return typeof token === "string" && token !== "" ? token : undefined;
}

let sessionToken: () => string | undefined = () => undefined;

/**
 * Where a browser signed in through a public address keeps its access token: in the page's
 * memory, handed over by the session store. The desktop shell's bootstrap token wins when there
 * is one, and nothing here is ever written to storage, a cookie or a URL.
 */
export function useSessionToken(source: () => string | undefined): void {
  sessionToken = source;
}

/** The token requests carry now: the shell's, else this browser's access token. */
export function activeToken(): string | undefined {
  return bootstrapToken() ?? sessionToken();
}

/** The request header carrying the token, or none without one. */
export function authHeaders(token: string | undefined): Record<string, string> {
  return token === undefined ? {} : { Authorization: `Bearer ${token}` };
}
