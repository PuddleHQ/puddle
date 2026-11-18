// SPDX-License-Identifier: AGPL-3.0-or-later
import type { PuddleBootstrap } from "#lib/api/connection.ts";

declare global {
  namespace App {
    interface Error {
      /** What was thrown, for an error no code expected (src/hooks.client.ts). */
      detail?: string;
    }
  }
  interface Window {
    /** Set before the app starts by the desktop shell's init script; never from a URL. */
    __PUDDLE__?: PuddleBootstrap;
  }
}

export {};
