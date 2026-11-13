// SPDX-License-Identifier: AGPL-3.0-or-later
import type { PuddleBootstrap } from "#lib/api/connection.ts";

declare global {
  interface Window {
    /** Set before the app starts by the desktop shell's init script; never from a URL. */
    __PUDDLE__?: PuddleBootstrap;
  }
}

export {};
