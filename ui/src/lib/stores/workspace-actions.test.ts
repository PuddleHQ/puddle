// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it } from "vitest";
import {
  FakeWorkspaces,
  cleanCheck,
  dirtyCheck,
  workspace,
} from "#lib/testing/fake-workspaces.ts";
import { ToastQueue } from "./toasts.svelte.ts";
import {
  WorkspaceActions,
  settledMessage,
} from "./workspace-actions.svelte.ts";
import { WorkspaceStore } from "./workspaces.svelte.ts";

let api: FakeWorkspaces;
let store: WorkspaceStore;
let toasts: ToastQueue;
let actions: WorkspaceActions;
let removed: { variables: number; secrets: number; rules: number } | null;

beforeEach(async () => {
  api = new FakeWorkspaces();
  api.list = [
    workspace("down"),
    workspace("up", { status: "running" }),
    workspace("trusted", { status: "running", direct_ssh: true }),
  ];
  store = new WorkspaceStore({ api: api as never });
  await store.refresh();
  toasts = new ToastQueue();
  removed = { variables: 2, secrets: 1, rules: 3 };
  actions = new WorkspaceActions(store, toasts, async () => removed);
});

const messages = () => toasts.items.map((t) => t.message);
const w = (name: string) => store.byId("w-" + name)!;

describe("start, stop and reclaim", () => {
  it("start and stop go to the service", async () => {
    await actions.start(w("down"));
    expect(api.calls).toContain("POST /api/workspaces/{id}/start");
    await actions.stop(w("trusted"));
    expect(api.calls).toContain("POST /api/workspaces/{id}/stop");
    await actions.reclaim(w("trusted"));
    expect(api.calls).toContain("POST /api/workspaces/{id}/reclaim");
    await actions.resetRootDisk(w("down"));
    expect(api.calls).toContain("POST /api/workspaces/{id}/reset-root-disk");
    expect(toasts.items).toHaveLength(0);
  });

  it("a refusal is an error toast that stays longer", async () => {
    api.refuse.set("POST /api/workspaces/{id}/start", {
      status: 409,
      message: "busy",
    });
    await actions.start(w("down"));
    expect(toasts.items).toMatchObject([{ message: "busy", tone: "error" }]);
    api.refuse.set("POST /api/workspaces/{id}/stop", {
      status: 409,
      message: "not running",
    });
    await actions.stop(w("down"));
    api.refuse.set("POST /api/workspaces/{id}/reclaim", {
      status: 409,
      message: "busy too",
    });
    await actions.reclaim(w("down"));
    api.refuse.set("POST /api/workspaces/{id}/reset-root-disk", {
      status: 409,
      message: "stop it first",
    });
    await actions.resetRootDisk(w("down"));
    expect(messages()).toEqual([
      "busy",
      "not running",
      "busy too",
      "stop it first",
    ]);
  });
});

describe("renaming", () => {
  it("opens the dialog for a workspace", () => {
    actions.openRename(w("up"));
    expect(actions.renameOpen).toBe(true);
    expect(actions.renaming?.id).toBe("w-up");
  });

  it("sends the trimmed name, shows it everywhere and says so", async () => {
    expect(await actions.submitRename(w("up"), "  Web shop ")).toBeNull();
    expect(api.bodies.at(-1)).toEqual({ name: "Web shop" });
    expect(w("up").name).toBe("Web shop");
    expect(messages()).toEqual(["Renamed to Web shop."]);
    // The words that follow use the new name.
    await actions.openDesktop(w("trusted"));
    expect(
      settledMessage({
        id: "w-up",
        label: w("up").name,
        operation: "stopping",
        failed: false,
        detail: null,
      }),
    ).toBe("Stopped Web shop.");
  });

  it("changes nothing, and asks nothing, when the name is the same", async () => {
    const before = api.calls.length;
    expect(await actions.submitRename(w("up"), " up ")).toBeNull();
    expect(api.calls).toHaveLength(before);
    expect(messages()).toEqual([]);
  });

  it("hands the service's refusal back to the dialog, with no toast", async () => {
    const message = await actions.submitRename(w("up"), "DOWN");
    expect(message).toContain("already called down");
    expect(messages()).toEqual([]);
    expect(w("up").name).toBe("up");
  });
});

describe("growing the root disk", () => {
  it("sends the size in MiB, marks the workspace busy and says nothing yet", async () => {
    api.list = [
      workspace("up", { status: "running", root_disk_size_mib: 10_240 }),
    ];
    await store.refresh();
    expect(await actions.growRootDisk(w("up"), 20)).toBeNull();
    expect(api.calls).toContain("POST /api/workspaces/{id}/grow-root-disk");
    expect(api.bodies.at(-1)).toEqual({ size_mib: 20_480 });
    expect(w("up").busy).toBe("growing_root_disk");
    expect(w("up").root_disk_size_mib).toBe(20_480);
    expect(messages()).toEqual([]);
  });

  it("hands the service's refusal back to the dialog, with no toast", async () => {
    api.list = [workspace("up", { root_disk_size_mib: 10_240 })];
    await store.refresh();
    const message = await actions.growRootDisk(w("up"), 5);
    expect(message).toContain("a root disk only grows");
    expect(w("up").busy).toBeNull();
    expect(messages()).toEqual([]);
  });

  it("says the service is not answering when it is down", async () => {
    api.down = true;
    expect(await actions.growRootDisk(w("up"), 20)).toBe(
      "Puddle's service isn't answering.",
    );
  });
});

describe("connecting", () => {
  it("opens the connect step for a workspace and opens nothing yet", () => {
    actions.connect(w("up"));
    expect(actions.connectOpen).toBe(true);
    expect(actions.connectFor?.name).toBe("up");
    expect(api.calls).not.toContain("POST /api/workspaces/{id}/attach");
  });

  it("turning direct SSH on asks the trust text first and changes nothing until the yes", async () => {
    actions.requestDirectSsh(w("up"), true);
    expect(actions.trustOpen).toBe(true);
    expect(actions.trustFor?.name).toBe("up");
    expect(api.calls).not.toContain("PUT /api/settings/workspaces/{workspace}");
    actions.confirmTrust();
    await expect
      .poll(() => messages())
      .toEqual(["Direct SSH is on for up: it is trusted now."]);
    expect(api.bodies.at(-1)).toMatchObject({
      overrides: { direct_ssh: true },
    });
    expect(w("up").direct_ssh).toBe(true);
    expect(actions.trustFor).toBeNull();
  });

  it("keeps the other overrides of the workspace when it turns direct SSH on", async () => {
    api.overrides["w-up"] = {
      capture_logins: null,
      warn_foreign_credentials: null,
      clipboard_read: null,
      direct_ssh: null,
      local_toggles: {
        link_local: null,
        loopback: null,
        metadata: null,
        private: null,
        special: null,
        company_network: null,
      },
      memory: 4096,
      cpus: null,
      reconnection_grace: null,
      wildcards_reach_local: null,
      zoom_hotkeys: null,
    };
    actions.requestDirectSsh(w("up"), true);
    actions.confirmTrust();
    await expect.poll(() => messages()).toHaveLength(1);
    expect(api.bodies.at(-1)).toMatchObject({
      overrides: { direct_ssh: true, memory: 4096 },
    });
  });

  it("turning it off needs no question and says so", async () => {
    actions.requestDirectSsh(w("trusted"), false);
    expect(actions.trustOpen).toBe(false);
    await expect
      .poll(() => messages())
      .toEqual(["Direct SSH is off for trusted."]);
    expect(api.bodies.at(-1)).toMatchObject({
      overrides: { direct_ssh: false },
    });
    expect(w("trusted").direct_ssh).toBe(false);
  });

  it("closes the connect step while the trust text shows, and brings it back after either answer", async () => {
    actions.connect(w("up"));
    actions.requestDirectSsh(w("up"), true, true);
    expect(actions.connectOpen).toBe(false);
    expect(actions.trustOpen).toBe(true);
    actions.cancelTrust();
    expect(actions.connectOpen).toBe(true);
    expect(actions.trustFor).toBeNull();
    actions.cancelTrust(); // a second cancel changes nothing
    actions.requestDirectSsh(w("up"), true, true);
    actions.confirmTrust();
    expect(actions.connectOpen).toBe(true);
    await expect.poll(() => messages()).toHaveLength(1);
  });

  it("a confirm with nothing to confirm does nothing", () => {
    actions.confirmTrust();
    expect(api.calls).not.toContain("PUT /api/settings/workspaces/{workspace}");
  });

  it("a refused change is an error toast and the workspace stays as it was", async () => {
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "storage failed",
    });
    actions.requestDirectSsh(w("up"), true);
    actions.confirmTrust();
    await expect
      .poll(() => toasts.items[0])
      .toMatchObject({
        message: "storage failed",
        tone: "error",
      });
    expect(w("up").direct_ssh).toBe(false);
  });

  it("says so when the settings cannot be read", async () => {
    api.refuse.set("GET /api/settings/workspaces/{workspace}", {
      status: 404,
      message: "no settings",
    });
    actions.requestDirectSsh(w("up"), true);
    actions.confirmTrust();
    await expect.poll(() => messages()).toEqual(["no settings"]);
  });

  it("says the service is down when it does not answer", async () => {
    api.down = true;
    actions.requestDirectSsh(w("up"), true);
    actions.confirmTrust();
    await expect
      .poll(() => messages())
      .toEqual(["Puddle's service isn't answering."]);
  });
});

describe("opening in VS Code", () => {
  it("opens desktop VS Code", async () => {
    actions.openDesktop(w("trusted"));
    await expect
      .poll(() => messages())
      .toEqual(["Opening trusted in VS Code."]);
    expect(api.bodies.at(-1)).toEqual({ mode: "desktop" });
  });

  it("says why when VS Code could not be opened, with the service's words", async () => {
    api.attachReply = {
      opened: false,
      message: "VS Code isn't installed.",
      open_uri: null,
    };
    actions.openDesktop(w("trusted"));
    await expect
      .poll(() => toasts.items[0])
      .toMatchObject({
        message: "VS Code isn't installed.",
        tone: "error",
      });
  });

  it("has words of its own when the service gave none", async () => {
    api.attachReply = { opened: false, message: null, open_uri: null };
    actions.openDesktop(w("trusted"));
    await expect
      .poll(() => messages())
      .toEqual(["Puddle couldn't open VS Code for trusted."]);
  });

  it("a refused attach (direct SSH off) is an error toast", async () => {
    api.refuse.set("POST /api/workspaces/{id}/attach", {
      status: 409,
      message: "direct SSH is off for this workspace; allow it first",
    });
    actions.openDesktop(w("up"));
    await expect
      .poll(() => messages())
      .toEqual(["direct SSH is off for this workspace; allow it first"]);
  });
});

describe("opening from another computer than the host", () => {
  const link = "vscode://vscode-remote/ssh-remote+puddle-up/workspaces/up";
  let followed: string[];
  beforeEach(() => {
    followed = [];
    actions = new WorkspaceActions(store, toasts, async () => removed, {
      assign: (uri: string | URL) => void followed.push(String(uri)),
    });
  });

  it("follows the link the host gives when it opened no editor itself", async () => {
    api.attachReply = {
      opened: false,
      message: "VS Code is not installed",
      open_uri: link,
    };
    actions.openDesktop(w("up"));
    await expect
      .poll(() => messages())
      .toEqual(["Opening up in VS Code on this computer."]);
    expect(followed).toEqual([link]);
    expect(toasts.items[0]?.tone).toBe("info");
  });

  it("follows nothing when the host opened the editor", async () => {
    api.attachReply = { opened: true, message: null, open_uri: link };
    actions.openDesktop(w("up"));
    await expect.poll(() => messages()).toEqual(["Opening up in VS Code."]);
    expect(followed).toEqual([]);
  });

  it("shows the host's words when it gave no link", async () => {
    api.attachReply = {
      opened: false,
      message: "direct SSH is on but the ssh config could not be written",
      open_uri: null,
    };
    actions.openDesktop(w("up"));
    await expect.poll(() => toasts.items[0]).toMatchObject({ tone: "error" });
    expect(followed).toEqual([]);
  });

  it("never follows a link for the browser editor", async () => {
    api.attachReply = { opened: false, message: "no window", open_uri: link };
    actions.openBrowser(w("up"));
    await expect
      .poll(() => toasts.items[0])
      .toMatchObject({ message: "no window" });
    expect(followed).toEqual([]);
  });
});

describe("opening in the browser", () => {
  it("asks for a browser attach and says the window is opening", async () => {
    actions.openBrowser(w("up"));
    await expect.poll(() => messages()).toEqual(["Opening up in the browser."]);
    expect(api.bodies.at(-1)).toEqual({ mode: "browser" });
    expect(toasts.items[0]?.tone).toBe("info");
  });

  it("shows the service's words when no window opened (a failed sign-in, no desktop app)", async () => {
    api.attachReply = {
      opened: false,
      message: "the browser editor opens from the desktop app",
      open_uri: null,
    };
    actions.openBrowser(w("up"));
    await expect
      .poll(() => toasts.items[0])
      .toMatchObject({
        message: "the browser editor opens from the desktop app",
        tone: "error",
      });
  });

  it("has words of its own when the service gave none, naming the browser editor", async () => {
    api.attachReply = { opened: false, message: null, open_uri: null };
    actions.openBrowser(w("up"));
    await expect
      .poll(() => messages())
      .toEqual(["Puddle couldn't open the browser editor for up."]);
  });

  it("a refused attach (not running) is an error toast with the service's words", async () => {
    api.refuse.set("POST /api/workspaces/{id}/attach", {
      status: 409,
      message: "down is not running; start it first",
    });
    actions.openBrowser(w("down"));
    await expect
      .poll(() => toasts.items[0])
      .toMatchObject({
        message: "down is not running; start it first",
        tone: "error",
      });
  });
});

describe("deleting", () => {
  it("reads the check, then asks with what it found", async () => {
    api.check = dirtyCheck("down");
    const asking = actions.askDelete(w("down"));
    expect(actions.checking).toBe("w-down");
    await asking;
    expect(actions.checking).toBeNull();
    expect(actions.deleteOpen).toBe(true);
    expect(actions.deleting?.check.fingerprint).toBe("fp-dirty");
  });

  it("counts what Puddle itself removes beside the disk, and again for a new list", async () => {
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    expect(actions.deleting?.removes).toEqual({
      variables: 2,
      secrets: 1,
      rules: 3,
    });
    removed = { variables: 0, secrets: 0, rules: 4 };
    api.check = dirtyCheck("down");
    api.refuse.set("DELETE /api/workspaces/{id}", {
      status: 409,
      message: "changed",
    });
    await actions.confirmDelete();
    expect(actions.deleting?.removes).toEqual({
      variables: 0,
      secrets: 0,
      rules: 4,
    });
    removed = null;
    await actions.askDelete(w("down"));
    expect(actions.deleting?.removes).toBeNull();
  });

  it("ignores a second ask while the first check is being read", async () => {
    api.check = cleanCheck("down");
    const first = actions.askDelete(w("down"));
    await actions.askDelete(w("down"));
    await first;
    expect(
      api.calls.filter((c) => c === "GET /api/workspaces/{id}/delete-check"),
    ).toHaveLength(1);
  });

  it("says so when the check can't be read", async () => {
    api.refuse.set("GET /api/workspaces/{id}/delete-check", {
      status: 409,
      message: "busy",
    });
    await actions.askDelete(w("down"));
    expect(actions.deleteOpen).toBe(false);
    expect(messages()).toEqual(["busy"]);
    expect(actions.checking).toBeNull();
  });

  it("deletes with the fingerprint it showed", async () => {
    api.check = dirtyCheck("down");
    await actions.askDelete(w("down"));
    await actions.confirmDelete();
    expect(api.bodies.at(-1)).toEqual({
      confirm: true,
      fingerprint: "fp-dirty",
    });
    expect(actions.deleteOpen).toBe(false);
    expect(messages()).toEqual(["Deleting down."]);
  });

  it("does nothing without a check, and nothing twice", async () => {
    await actions.confirmDelete();
    expect(api.calls).not.toContain("DELETE /api/workspaces/{id}");
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    const first = actions.confirmDelete();
    await actions.confirmDelete();
    await first;
    expect(api.calls.filter((c) => c.startsWith("DELETE"))).toHaveLength(1);
  });

  it("shows the new list when the workspace changed since the check", async () => {
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    api.check = dirtyCheck("down");
    api.refuse.set("DELETE /api/workspaces/{id}", {
      status: 409,
      message: "changed since you looked",
    });
    await actions.confirmDelete();
    expect(actions.deleteOpen).toBe(true);
    expect(actions.deleting).toMatchObject({
      working: false,
      error: "changed since you looked",
      check: { fingerprint: "fp-dirty" },
    });
    await actions.confirmDelete();
    expect(api.bodies.at(-1)).toEqual({
      confirm: true,
      fingerprint: "fp-dirty",
    });
    expect(actions.deleteOpen).toBe(false);
  });

  it("keeps the old list and the reason when it can't read the new one", async () => {
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    api.refuse.set("DELETE /api/workspaces/{id}", {
      status: 409,
      message: "changed",
    });
    api.refuse.set("GET /api/workspaces/{id}/delete-check", {
      status: 500,
      message: "x",
    });
    await actions.confirmDelete();
    expect(actions.deleting).toMatchObject({
      working: false,
      error: "changed",
      check: { fingerprint: "fp-clean" },
    });
  });

  it("closes and says so when the workspace is gone already", async () => {
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    api.list = api.list.filter((x) => x.name !== "down");
    await actions.confirmDelete();
    expect(actions.deleteOpen).toBe(false);
    expect(messages()).toEqual(["That workspace no longer exists."]);
  });

  it("keeps the dialog open with the reason after another refusal", async () => {
    api.check = cleanCheck("down");
    await actions.askDelete(w("down"));
    api.refuse.set("DELETE /api/workspaces/{id}", {
      status: 422,
      message: "confirm must be true",
    });
    await actions.confirmDelete();
    expect(actions.deleting).toMatchObject({
      working: false,
      error: "confirm must be true",
    });
    expect(actions.deleteOpen).toBe(true);
  });
});

describe("the rest", () => {
  it("opens the create dialog", () => {
    actions.openCreate();
    expect(actions.createOpen).toBe(true);
  });

  it("dismisses a failure the user saw", () => {
    store.progress = {
      "w-down": {
        step: "failed",
        detail: "x",
        repository: null,
        operation: null,
        failed: true,
      },
    };
    actions.dismissProgress(w("down"));
    expect(store.progress["w-down"]).toBeUndefined();
  });

  it("toasts the end of an operation, and a failure as an error", () => {
    actions.settled({
      id: "w-up",
      label: "up",
      operation: "starting",
      failed: false,
      detail: null,
    });
    actions.settled({
      id: "w-up",
      label: "up",
      operation: "stopping",
      failed: true,
      detail: "no",
    });
    expect(toasts.items).toMatchObject([
      { message: "up is running.", tone: "info" },
      { message: "Stopping up failed: no", tone: "error" },
    ]);
  });
});

describe("settledMessage", () => {
  it.each([
    [{ operation: "creating", failed: false, detail: null }, "Created w."],
    [{ operation: "starting", failed: false, detail: null }, "w is running."],
    [{ operation: "stopping", failed: false, detail: null }, "Stopped w."],
    [
      { operation: "reclaiming", failed: false, detail: null },
      "Reclaimed free space on w.",
    ],
    [
      { operation: "resetting_root_disk", failed: false, detail: null },
      "Reset the root disk of w; it is made again at the next start.",
    ],
    [
      { operation: "growing_root_disk", failed: false, detail: null },
      "Made the root disk of w bigger.",
    ],
    [
      { operation: "cloning", failed: false, detail: null },
      "Cloned a repository into w.",
    ],
    [{ operation: "deleting", failed: false, detail: null }, "Deleted w."],
    [{ operation: null, failed: false, detail: null }, "w: done."],
    [
      { operation: "creating", failed: true, detail: "no repo" },
      "Creating w failed: no repo",
    ],
    [
      { operation: "deleting", failed: true, detail: null },
      "Deleting w failed.",
    ],
    [
      { operation: null, failed: true, detail: "x" },
      "Something went wrong with w: x",
    ],
    [
      { operation: null, failed: true, detail: null },
      "Something went wrong with w.",
    ],
  ] as const)("%j", (rest, text) => {
    expect(settledMessage({ id: "w-w", label: "w", ...rest })).toBe(text);
  });

  it("speaks of the workspace by the name the user gave it", () => {
    const settled = {
      id: "w-w",
      label: "Web shop",
      operation: "starting",
      failed: false,
      detail: null,
    } as const;
    expect(settledMessage(settled)).toBe("Web shop is running.");
    expect(settledMessage({ ...settled, failed: true })).toBe(
      "Starting Web shop failed.",
    );
  });
});
