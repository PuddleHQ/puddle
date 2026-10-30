// SPDX-License-Identifier: GPL-3.0-or-later
// What a click on a workspace does, shared by the list and the detail page: start, stop,
// reclaim, connect (the "how do you want to connect" step, with the direct-SSH switch and its
// trust text), delete (with the check of what would be lost) and the create dialog. The dialogs themselves are drawn by
// `WorkspaceDialogs.svelte`, which reads the state here.
import {
  workspaces as defaultStore,
  type Settled,
  type WorkspaceStore,
} from "./workspaces.svelte.ts";
import { toasts as defaultToasts, type ToastQueue } from "./toasts.svelte.ts";
import { readRemoved, type Removed } from "#lib/workspaces/removed.ts";
import {
  operationLabel,
  type DeleteCheck,
  type Workspace,
} from "#lib/workspaces/model.ts";

export interface DeleteState {
  workspace: Workspace;
  check: DeleteCheck;
  /** What Puddle removes from its own records besides the disk; `null` when it could not be counted. */
  removes: Removed | null;
  /** A delete request is in flight. */
  working: boolean;
  /** Why the last delete request failed, when it did (a changed workspace shows the new list). */
  error: string | null;
}

type Toasts = Pick<ToastQueue, "push">;
type Store = Pick<
  WorkspaceStore,
  | "startWorkspace"
  | "stopWorkspace"
  | "reclaim"
  | "rename"
  | "attach"
  | "setDirectSsh"
  | "checkDelete"
  | "remove"
  | "refresh"
  | "dismissProgress"
>;

const DONE_WORDS: Record<string, (label: string) => string> = {
  creating: (name) => `Created ${name}.`,
  starting: (name) => `${name} is running.`,
  stopping: (name) => `Stopped ${name}.`,
  reclaiming: (name) => `Reclaimed free space on ${name}.`,
  cloning: (name) => `Cloned a repository into ${name}.`,
  deleting: (name) => `Deleted ${name}.`,
};

/** The toast for a finished operation. */
export function settledMessage(settled: Settled): string {
  if (settled.failed) {
    const what = settled.operation
      ? `${operationLabel(settled.operation)} ${settled.label} failed`
      : `Something went wrong with ${settled.label}`;
    return settled.detail ? `${what}: ${settled.detail}` : `${what}.`;
  }
  return (DONE_WORDS[settled.operation ?? ""] ?? ((n) => `${n}: done.`))(
    settled.label,
  );
}

/** How each way of opening is named in a toast. */
const EDITOR_WORDS = {
  browser: { where: "the browser", what: "the browser editor" },
  desktop: { where: "VS Code", what: "VS Code" },
} as const;

export class WorkspaceActions {
  createOpen = $state(false);
  /** The workspace the connect step is open for (a snapshot: read live data by its id). */
  connectFor = $state.raw<Workspace | null>(null);
  connectOpen = $state(false);
  /** The workspace whose direct-SSH trust text is being asked. */
  trustFor = $state.raw<Workspace | null>(null);
  trustOpen = $state(false);
  /** The trust text was asked from the connect step, which closes while it is shown. */
  #backToConnect = false;
  /** The workspace the rename dialog is open for (a snapshot: read live data by its id). */
  renaming = $state.raw<Workspace | null>(null);
  renameOpen = $state(false);
  deleting = $state.raw<DeleteState | null>(null);
  deleteOpen = $state(false);
  /** The workspace whose delete check is being read. */
  checking = $state<string | null>(null);

  readonly #store: Store;
  readonly #toasts: Toasts;

  readonly #removed: (name: string) => Promise<Removed | null>;

  constructor(
    store: Store = defaultStore,
    toasts: Toasts = defaultToasts,
    removed: (name: string) => Promise<Removed | null> = readRemoved,
  ) {
    this.#store = store;
    this.#toasts = toasts;
    this.#removed = removed;
  }

  #fail(message: string): void {
    this.#toasts.push(message, { tone: "error", ms: 8000 });
  }

  /** The toast for an operation's end; the store calls it. */
  settled = (settled: Settled): void => {
    this.#toasts.push(settledMessage(settled), {
      tone: settled.failed ? "error" : "info",
      ms: settled.failed ? 8000 : 5000,
    });
  };

  openCreate = (): void => {
    this.createOpen = true;
  };

  start = async (w: Workspace): Promise<void> => {
    const result = await this.#store.startWorkspace(w.id);
    if (!result.ok) this.#fail(result.message);
  };

  stop = async (w: Workspace): Promise<void> => {
    const result = await this.#store.stopWorkspace(w.id);
    if (!result.ok) this.#fail(result.message);
  };

  reclaim = async (w: Workspace): Promise<void> => {
    const result = await this.#store.reclaim(w.id);
    if (!result.ok) this.#fail(result.message);
  };

  openRename = (w: Workspace): void => {
    this.renaming = w;
    this.renameOpen = true;
  };

  /**
   * Renames to what the user typed; a name equal to the workspace's own name goes back to it.
   * Returns the refusal as text for the dialog, or `null` when it went through.
   */
  submitRename = async (w: Workspace, text: string): Promise<string | null> => {
    const shown = text.trim();
    if (shown === w.display_name) return null;
    const result = await this.#store.rename(
      w.id,
      shown === w.name ? null : shown,
    );
    if (!result.ok) return result.message;
    this.#toasts.push(`Renamed to ${result.value.display_name}.`);
    return null;
  };

  /** Opens the connect step. */
  connect = (w: Workspace): void => {
    this.connectFor = w;
    this.connectOpen = true;
  };

  /** The switch in the connect step or the settings: off at once, on after the trust text. */
  requestDirectSsh = (w: Workspace, on: boolean, fromConnect = false): void => {
    if (on) {
      this.trustFor = w;
      this.trustOpen = true;
      // One dialog at a time: the connect step comes back once the question is answered.
      this.#backToConnect = fromConnect;
      if (fromConnect) this.connectOpen = false;
      return;
    }
    void this.#setDirectSsh(w, false);
  };

  confirmTrust = (): void => {
    const w = this.trustFor;
    this.trustFor = null;
    if (w) void this.#setDirectSsh(w, true);
    this.#reopenConnect();
  };

  /** The trust text was cancelled (or closed): nothing changes. */
  cancelTrust = (): void => {
    if (!this.trustFor) return; // already answered
    this.trustFor = null;
    this.#reopenConnect();
  };

  #reopenConnect(): void {
    if (this.#backToConnect) this.connectOpen = true;
    this.#backToConnect = false;
  }

  async #setDirectSsh(w: Workspace, on: boolean): Promise<void> {
    const result = await this.#store.setDirectSsh(w.name, on);
    if (!result.ok) {
      this.#fail(result.message);
      return;
    }
    this.#toasts.push(
      on
        ? `Direct SSH is on for ${w.display_name}: it is trusted now.`
        : `Direct SSH is off for ${w.display_name}.`,
    );
  }

  /** Opens the browser editor's window; it needs no direct SSH. */
  openBrowser = (w: Workspace): void => {
    void this.#open(w, "browser");
  };

  /** Opens desktop VS Code; the connect step only offers it while direct SSH is on. */
  openDesktop = (w: Workspace): void => {
    void this.#open(w, "desktop");
  };

  async #open(w: Workspace, mode: "browser" | "desktop"): Promise<void> {
    const words = EDITOR_WORDS[mode];
    const result = await this.#store.attach(w.id, mode);
    if (!result.ok) {
      this.#fail(result.message);
      return;
    }
    const { opened, message } = result.value;
    if (opened) {
      this.#toasts.push(`Opening ${w.display_name} in ${words.where}.`);
    } else {
      this.#fail(
        message ?? `Puddle couldn't open ${words.what} for ${w.display_name}.`,
      );
    }
  }

  /** Reads what deleting would lose, then asks. */
  askDelete = async (w: Workspace): Promise<void> => {
    if (this.checking !== null) return;
    this.checking = w.name;
    const result = await this.#store.checkDelete(w.id);
    this.checking = null;
    if (!result.ok) {
      this.#fail(result.message);
      return;
    }
    this.deleting = {
      workspace: w,
      check: result.value,
      removes: await this.#removed(w.name),
      working: false,
      error: null,
    };
    this.deleteOpen = true;
  };

  /** Deletes what the user saw listed. If it changed meanwhile, shows the new list instead. */
  confirmDelete = async (): Promise<void> => {
    const state = this.deleting;
    if (!state || state.working) return;
    this.deleting = { ...state, working: true, error: null };
    const result = await this.#store.remove(
      state.workspace.id,
      state.check.fingerprint,
    );
    if (result.ok) {
      this.deleteOpen = false;
      this.#toasts.push(`Deleting ${state.workspace.display_name}.`);
      return;
    }
    if (result.reason === "gone") {
      this.deleteOpen = false;
      this.#fail(result.message);
      return;
    }
    if (result.reason === "conflict") {
      const again = await this.#store.checkDelete(state.workspace.id);
      if (again.ok) {
        this.deleting = {
          ...state,
          check: again.value,
          removes: await this.#removed(state.workspace.name),
          working: false,
          error: result.message,
        };
        return;
      }
    }
    this.deleting = { ...state, working: false, error: result.message };
  };

  dismissProgress = (w: Workspace): void => {
    this.#store.dismissProgress(w.name);
  };
}

export const workspaceActions = new WorkspaceActions();
