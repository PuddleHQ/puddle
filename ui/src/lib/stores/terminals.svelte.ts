// SPDX-License-Identifier: AGPL-3.0-or-later
/* eslint-disable svelte/prefer-svelte-reactivity -- the forwards map is read when a link is hovered or clicked, never by a template */
// The page's terminals, by workspace: the sub-tabs of the Terminal tab, which one is in view and
// the limits on how many are open. A terminal lives as long as the page: leaving the tab keeps it, a
// page that loads again starts with none.
import {
  TerminalSession,
  type SessionDeps,
} from "#lib/terminal/session.svelte.ts";
import { XtermScreen, type Screen } from "#lib/terminal/xterm-screen.ts";
import type { PortEntry } from "#lib/workspaces/ports.ts";
import { MAX_IN_ALL, MAX_PER_WORKSPACE } from "#lib/terminal/wire.ts";

/** Which limit forbids another terminal, in words; `null` when there is room. */
export function limitReason(inWorkspace: number, inAll: number): string | null {
  if (inWorkspace >= MAX_PER_WORKSPACE) {
    return `This workspace has ${MAX_PER_WORKSPACE} terminals open, the most one can have.`;
  }
  if (inAll >= MAX_IN_ALL) {
    return `${MAX_IN_ALL} terminals are open in all, the most Puddle can have.`;
  }
  return null;
}

export class TerminalStore {
  sessions = $state.raw<TerminalSession[]>([]);
  /** The sub-tab in view, by workspace: the session's number. */
  current = $state<Record<string, number>>({});
  /** Why the last New terminal did not open one. */
  problem = $state<string | null>(null);
  /** The tab's Screen reader mode switch: the emulator's accessible text buffer. */
  screenReader = $state(false);
  readonly #made: (workspace: string, number: number) => TerminalSession;
  readonly #numbers: Record<string, number> = {};

  /**
   * `makeScreen` builds the emulator of a new terminal; `deps` is for tests.
   */
  constructor(
    makeScreen: (workspace: string) => Screen,
    deps: Partial<SessionDeps> = {},
  ) {
    this.#made = (workspace, number) =>
      new TerminalSession(workspace, number, makeScreen(workspace), deps);
  }

  of(workspace: string): TerminalSession[] {
    return this.sessions.filter((s) => s.workspace === workspace);
  }

  /** The sub-tab in view for `workspace`: the chosen one if it is still there, else the last. */
  selected(workspace: string): TerminalSession | undefined {
    const own = this.of(workspace);
    return own.find((s) => s.number === this.current[workspace]) ?? own.at(-1);
  }

  /** The reason New terminal is off for `workspace`, or `null`. */
  limit(workspace: string): string | null {
    const open = (list: TerminalSession[]) => list.filter((s) => s.open).length;
    return limitReason(open(this.of(workspace)), open(this.sessions));
  }

  select(workspace: string, session: TerminalSession): void {
    this.current[workspace] = session.number;
    session.bell = false;
  }

  /** Adds a sub-tab and puts it in view; it connects when its view reports a size. */
  add(workspace: string): TerminalSession | null {
    const reason = this.limit(workspace);
    if (reason !== null) {
      this.problem = reason;
      return null;
    }
    this.problem = null;
    const number = (this.#numbers[workspace] ?? 0) + 1;
    this.#numbers[workspace] = number;
    const session = this.#made(workspace, number);
    this.sessions = [...this.sessions, session];
    this.select(workspace, session);
    return session;
  }

  /** Opens the socket of a session that was never asked for, with the size its view measured. */
  async connect(
    session: TerminalSession,
    size: { cols: number; rows: number },
  ): Promise<void> {
    await session.connect(size);
    if (session.refused) {
      // A first open the host refused: no sub-tab, and the reason on the tab's toolbar.
      this.problem = session.end?.line ?? null;
      this.remove(session);
    }
  }

  /** Closes a sub-tab: the host ends its session and what it started. */
  remove(session: TerminalSession): void {
    session.close();
    this.sessions = this.sessions.filter((s) => s !== session);
  }
}

/** What each workspace forwards, as its Ports list last said; a link to this computer is judged by it. */
const forwardsMap = new Map<string, readonly PortEntry[]>();

/** Tells the links in `workspace`'s terminals what the workspace forwards. */
export function setForwards(
  workspace: string,
  list: readonly PortEntry[],
): void {
  forwardsMap.set(workspace, list);
}

/** Opens `url` in the default browser: the shell turns a new window into that. */
function openInBrowser(url: string): void {
  globalThis.open(url, "_blank", "noopener,noreferrer");
}

/** The page's terminals. */
export const terminals = new TerminalStore(
  (workspace) =>
    new XtermScreen({
      forwards: () => forwardsMap.get(workspace) ?? [],
      open: openInBrowser,
    }),
);
