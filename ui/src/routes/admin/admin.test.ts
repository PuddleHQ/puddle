// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const store = vi.hoisted(() => ({
  status: "ready" as string,
  reason: null as string | null,
  users: [] as unknown[],
  stats: null as unknown,
  audit: [] as unknown[],
  load: vi.fn(async () => undefined),
  create: vi.fn(),
  reissue: vi.fn(),
  remove: vi.fn(),
}));
vi.mock("#lib/stores/admin.svelte.ts", () => ({ admin: store }));

import { user } from "#lib/testing/fake-admin.ts";
import Page from "./+page.svelte";

const owner = user({ id: 1, name: "Owen", role: "owner", last_sign_in: 5 });
const stats = {
  users: 2,
  workspaces: 3,
  running: 1,
  disk_used_mib: 2048,
  memory_mib: 512,
  host_cpus: 8,
};

beforeEach(() => {
  store.status = "ready";
  store.reason = null;
  store.users = [owner, user({ workspaces_running: 1, devices: 2 })];
  store.stats = stats;
  store.audit = [
    {
      action: "user_created",
      actor: "Owen",
      target: "Sam",
      target_id: 2,
      at: 1,
    },
  ];
  store.load.mockClear();
  store.create.mockReset();
  store.reissue.mockReset();
  store.remove.mockReset();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const invite = {
  invite_secret: "s3cret",
  expires_at: 1_800_000_000_000,
  user: user(),
};

describe("the admin page", () => {
  it("reads when it opens, puts focus on the heading and shows the totals", () => {
    render(Page);
    expect(store.load).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(
      screen.getByRole("heading", { level: 1, name: "Admin" }),
    );
    const totals = screen.getByRole("region", { name: "This server" });
    expect(totals).toHaveTextContent(/Disk in use\s*2 GiB/);
    expect(totals).toHaveTextContent(/Processors\s*8/);
    expect(totals).toHaveTextContent("no per-user limits");
  });

  it("lists each user with counts only, and offers actions on others, not the owner", () => {
    render(Page);
    const rows = within(
      screen.getByRole("list", { name: "Users" }),
    ).getAllByRole("listitem");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent("Owen");
    expect(rows[0]).toHaveTextContent("Owner");
    expect(within(rows[0]!).queryByRole("button")).toBeNull();
    expect(rows[1]).toHaveTextContent("1 running");
    expect(rows[1]).toHaveTextContent("never");
    expect(
      within(rows[1]!).getByRole("button", { name: "Reset password for Sam" }),
    ).toBeInTheDocument();
    expect(
      within(rows[1]!).getByRole("button", { name: "Delete Sam" }),
    ).toBeInTheDocument();
  });

  it("shows an empty state when nobody else has an account, and what was done", () => {
    store.users = [owner];
    render(Page);
    expect(
      screen.getByText(/Nobody else has an account yet/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Owen made the account Sam/)).toBeInTheDocument();
  });

  it("says nothing was done yet when the audit is empty", () => {
    store.audit = [];
    store.stats = null;
    render(Page);
    expect(screen.getByText("Nothing yet.")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "This server" })).toBeNull();
  });

  it("shows loading and a failure with a way to try again", async () => {
    store.status = "loading";
    render(Page);
    expect(screen.getByText(/Loading/)).toBeInTheDocument();
    cleanup();
    store.status = "failed";
    store.reason = "Puddle's service isn't answering.";
    render(Page);
    expect(
      screen.getByText("Couldn't read the admin data."),
    ).toBeInTheDocument();
    expect(screen.getByText(/isn't answering/)).toBeInTheDocument();
    store.load.mockClear();
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(store.load).toHaveBeenCalledTimes(1);
  });

  it("makes a user and shows the one-time link", async () => {
    store.create.mockResolvedValue({ ok: true, invite });
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "Add user" }));
    await fireEvent.input(await screen.findByLabelText("Name"), {
      target: { value: "Sam" },
    });
    await fireEvent.click(
      screen.getAllByRole("button", { name: "Add user" }).at(-1)!,
    );
    expect(store.create).toHaveBeenCalledWith("Sam");
    expect(
      await screen.findByRole("dialog", { name: "Invite link for Sam" }),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Invite link")).toHaveValue(
      `${window.location.origin}/invite#s3cret`,
    );
    await fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.body.textContent).not.toContain("s3cret");
  });

  it("closes the add dialog on Cancel without making anyone", async () => {
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "Add user" }));
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(store.create).not.toHaveBeenCalled();
  });

  it("keeps the add dialog open with the reason when the create is refused", async () => {
    store.create.mockResolvedValue({
      ok: false,
      message: "That name is taken.",
    });
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "Add user" }));
    await fireEvent.input(await screen.findByLabelText("Name"), {
      target: { value: "Sam" },
    });
    await fireEvent.click(
      screen.getAllByRole("button", { name: "Add user" }).at(-1)!,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "That name is taken.",
    );
    expect(
      screen.getByRole("dialog", { name: "Add a user" }),
    ).toBeInTheDocument();
  });

  it("gives a new invite link, or a toast when that is refused", async () => {
    store.reissue.mockResolvedValueOnce({ ok: true, invite });
    render(Page);
    await fireEvent.click(
      screen.getByRole("button", { name: "Reset password for Sam" }),
    );
    expect(store.reissue).toHaveBeenCalledWith(2);
    expect(
      await screen.findByRole("dialog", { name: "Invite link for Sam" }),
    ).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    store.reissue.mockResolvedValueOnce({ ok: false, message: "Not now." });
    await fireEvent.click(
      screen.getByRole("button", { name: "Reset password for Sam" }),
    );
    expect(await screen.findByText("Not now.")).toBeInTheDocument();
  });

  it("deletes only after the name is typed", async () => {
    store.remove.mockResolvedValue({ ok: true });
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "Delete Sam" }));
    const dialog = await screen.findByRole("alertdialog", {
      name: "Delete Sam?",
    });
    await fireEvent.input(within(dialog).getByLabelText(/Type/), {
      target: { value: "Sam" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete user" }),
    );
    expect(store.remove).toHaveBeenCalledWith(2, "Sam");
    expect(await screen.findByText("Deleting Sam.")).toBeInTheDocument();
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
  });

  it("keeps the delete dialog open with the reason, and forgets it on Cancel", async () => {
    store.remove.mockResolvedValue({
      ok: false,
      message: "The name does not match.",
    });
    render(Page);
    await fireEvent.click(screen.getByRole("button", { name: "Delete Sam" }));
    const dialog = await screen.findByRole("alertdialog");
    await fireEvent.input(within(dialog).getByLabelText(/Type/), {
      target: { value: "Sam" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Delete user" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "does not match",
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    await fireEvent.click(screen.getByRole("button", { name: "Delete Sam" }));
    expect(await screen.findByRole("alertdialog")).not.toHaveTextContent(
      "does not match",
    );
  });

  it("looks again while a deletion is under way, and not otherwise", async () => {
    vi.useFakeTimers();
    store.users = [owner, user({ state: "deleting" })];
    render(Page);
    expect(screen.queryByRole("button", { name: /Delete Sam/ })).toBeNull();
    store.load.mockClear();
    await vi.advanceTimersByTimeAsync(5000);
    expect(store.load).toHaveBeenCalledTimes(1);
    store.users = [owner, user()];
    await vi.advanceTimersByTimeAsync(5000);
    expect(store.load).toHaveBeenCalledTimes(1);
  });
});
