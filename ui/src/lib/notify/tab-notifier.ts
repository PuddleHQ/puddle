// SPDX-License-Identifier: AGPL-3.0-or-later
// Raises a Web Notification in this tab when the service picked it for a new request. The
// service decides (`pending_notify` names the tabs), so a person with several tabs, a phone and
// the desktop app is told once. One notification per request and device: the tag is the request
// id, so a second tab of the same browser replaces the first.
//
// A notification says what the desktop app's says and nothing more: the workspace's name, and
// the host and port the request is for, with control and markup characters replaced. Nothing
// else a workspace sent ever reaches a notification, and a click opens only Puddle's own inbox.

/** The longest name or host a notification quotes (as the desktop app's). */
const MAX_QUOTED_CHARS = 80;

/** A name or host from a workspace, made safe to show: control, format (direction marks) and markup characters become `?`, and a long one is cut. */
export function cleanQuoted(text: string): string {
  const chars = Array.from(text);
  const out = chars
    .slice(0, MAX_QUOTED_CHARS)
    .map((c) => (/[\p{Cc}\p{Cf}\p{Zl}\p{Zp}<>&]/u.test(c) ? "?" : c))
    .join("");
  return chars.length > MAX_QUOTED_CHARS ? `${out}…` : out;
}

/** The notification for a request. */
export function requestNotice(
  workspace: string,
  host: string,
  port: number,
): { title: string; body: string } {
  return {
    title: `${cleanQuoted(workspace)} is asking for access`,
    body: `${cleanQuoted(host)}:${port} is waiting for your decision.`,
  };
}

/** The part of a `Notification` this uses. */
export interface ShownNotification {
  onclick: ((event: Event) => void) | null;
  close(): void;
}

export interface TabNotifierDeps {
  /** This tab's client id, as it reported it. */
  clientId: string;
  /** Whether this browser's switch is on and the permission given. */
  active: () => boolean;
  /** What the person calls a workspace. */
  labelOf: (workspace: string) => string;
  create: (
    title: string,
    options: { body: string; tag: string; data: { request: number } },
  ) => ShownNotification;
  /** Brings the tab forward and opens `path` in it. */
  open: (path: string) => void;
}

/** The path a click opens: the inbox at that request. */
export const inboxPath = (request: number) => `/inbox?request=${request}`;

const MAX_SHOWN = 50;

const isObj = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

export class TabNotifier {
  readonly #deps: TabNotifierDeps;
  readonly #shown = new Map<number, ShownNotification>();

  constructor(deps: TabNotifierDeps) {
    this.#deps = deps;
  }

  /** Applies one stream event; true when it was one of this notifier's. */
  handle(raw: unknown): boolean {
    if (!isObj(raw)) return false;
    if (raw["type"] === "pending_closed") {
      const id = raw["id"];
      if (typeof id === "number") this.#withdraw(id);
      return false;
    }
    if (raw["type"] !== "pending_notify") return false;
    const request = raw["request"];
    const clients = raw["clients"];
    if (
      !isObj(request) ||
      !Array.isArray(clients) ||
      typeof request["id"] !== "number" ||
      typeof request["workspace"] !== "string" ||
      typeof request["host"] !== "string" ||
      typeof request["port"] !== "number"
    ) {
      return true;
    }
    if (!clients.includes(this.#deps.clientId) || !this.#deps.active()) {
      return true;
    }
    const id = request["id"];
    const text = requestNotice(
      this.#deps.labelOf(request["workspace"]),
      request["host"],
      request["port"],
    );
    try {
      const shown = this.#deps.create(text.title, {
        body: text.body,
        tag: `puddle-request-${id}`,
        data: { request: id },
      });
      shown.onclick = () => {
        shown.close();
        this.#shown.delete(id);
        this.#deps.open(inboxPath(id));
      };
      this.#remember(id, shown);
    } catch {
      // A browser that refuses (a page that may not notify): the inbox has the request.
    }
    return true;
  }

  #remember(id: number, shown: ShownNotification): void {
    this.#shown.set(id, shown);
    while (this.#shown.size > MAX_SHOWN) {
      // A map over the limit has an oldest key.
      this.#shown.delete(this.#shown.keys().next().value as number);
    }
  }

  /** A request that was decided or expired no longer needs its notification. */
  #withdraw(id: number): void {
    this.#shown.get(id)?.close();
    this.#shown.delete(id);
  }
}
