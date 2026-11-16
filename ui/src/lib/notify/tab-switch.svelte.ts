// SPDX-License-Identifier: AGPL-3.0-or-later
// This browser's own switch for notifications about new requests. It belongs to the browser, not
// to the install: a phone and a desktop browser each choose. Turning it on asks the browser for
// permission (which needs the click that calls `enable`); it is "active" only while both the
// switch and the permission are on.

const KEY = "puddle.tab-notifications";

/** The part of the browser's `Notification` this needs. */
export interface PermissionApi {
  readonly permission: NotificationPermission;
  requestPermission(): Promise<NotificationPermission>;
}

/** What turning the switch on came to. */
export type EnableResult = "on" | "denied" | "unsupported";

export class TabNotificationSwitch {
  /** The person's choice, kept in this browser. */
  chosen = $state(false);
  /** The browser's permission as last read; `unsupported` where there is no `Notification`. */
  permission = $state<NotificationPermission | "unsupported">("unsupported");

  readonly #storage: Pick<Storage, "getItem" | "setItem"> | undefined;
  readonly #given: PermissionApi | null | undefined;

  /** `api`: the browser's own when left out; `null` for a browser with none. */
  constructor(
    storage: Pick<Storage, "getItem" | "setItem"> | undefined = safeStorage(),
    api?: PermissionApi | null,
  ) {
    this.#storage = storage;
    this.#given = api;
    try {
      this.chosen = storage?.getItem(KEY) === "on";
    } catch {
      this.chosen = false;
    }
    this.permission = this.#api?.permission ?? "unsupported";
  }

  /** Looked up when used, so a page that gets `Notification` late still has it. */
  get #api(): PermissionApi | undefined {
    return this.#given === undefined
      ? browserApi()
      : (this.#given ?? undefined);
  }

  /** Whether a notification may be shown: chosen, and the browser allows it. */
  get active(): boolean {
    return this.chosen && this.permission === "granted";
  }

  /** Turns the switch on: asks for the permission when it is not yet given. */
  async enable(): Promise<EnableResult> {
    const api = this.#api;
    if (!api) return "unsupported";
    if (api.permission !== "granted") {
      try {
        await api.requestPermission();
      } catch {
        // Read below.
      }
    }
    this.permission = api.permission;
    if (this.permission !== "granted") {
      this.#keep(false);
      return "denied";
    }
    this.#keep(true);
    return "on";
  }

  disable(): void {
    this.#keep(false);
  }

  /** Reads the permission again: the person may have changed it in the browser's own settings. */
  refresh(): void {
    this.permission = this.#api?.permission ?? "unsupported";
  }

  #keep(on: boolean): void {
    this.chosen = on;
    try {
      this.#storage?.setItem(KEY, on ? "on" : "off");
    } catch {
      // Lasts until the page is closed.
    }
  }
}

function safeStorage(): Storage | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

function browserApi(): PermissionApi | undefined {
  return typeof Notification === "undefined" ? undefined : Notification;
}

/** This browser's switch. */
export const tabNotifications = new TabNotificationSwitch();
