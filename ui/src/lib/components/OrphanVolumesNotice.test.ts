// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OrphanVolumes } from "#lib/stores/orphan-volumes.svelte.ts";
import OrphanVolumesNotice from "./OrphanVolumesNotice.svelte";

afterEach(cleanup);

type Reply = { data?: unknown; error?: { message: string }; status: number };

async function mount(volumes: string[], post: Reply[] = []) {
  const reply = (r: Reply | undefined) => ({
    data: r?.data,
    error: r?.error,
    response: { status: r?.status ?? 200 },
  });
  const api = {
    GET: vi.fn(async () =>
      reply({
        status: 200,
        data: { volumes: volumes.map((id) => ({ id, volume: `ws-${id}` })) },
      }),
    ),
    POST: vi.fn(async () => reply(post.shift())),
  };
  const store = new OrphanVolumes(api as never, undefined);
  await store.load();
  render(OrphanVolumesNotice, { props: { store } } as never);
  return { api, store };
}

describe("the unclaimed workspace disks notice", () => {
  it("says nothing when the list names every disk", async () => {
    await mount([]);
    expect(screen.queryByRole("region")).toBeNull();
  });

  it("names each disk, says Puddle kept it, and never claims it holds files", async () => {
    await mount(["lost", "other"]);
    const notice = screen.getByRole("region", {
      name: "2 workspace disks are not in your workspace list",
    });
    expect(notice).toHaveTextContent("ws-lost");
    expect(notice).toHaveTextContent("ws-other");
    expect(notice).toHaveTextContent("Puddle kept them and changed nothing");
    expect(notice).toHaveTextContent("cannot tell from outside");
    expect(
      screen.getAllByRole("button", { name: /Restore as workspace/ }),
    ).toHaveLength(2);
    expect(screen.queryByRole("button", { name: /delete|remove/i })).toBeNull();
  });

  it("restores a disk and moves focus to the notice, which still names the other disk", async () => {
    const { api } = await mount(
      ["lost", "other"],
      [{ status: 201, data: { name: "lost" } }],
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Restore as workspace ws-lost" }),
    );
    expect(api.POST).toHaveBeenCalledOnce();
    await vi.waitFor(() => expect(screen.queryByText("ws-lost")).toBeNull());
    expect(screen.getByText("ws-other")).toBeTruthy();
    expect(screen.getByRole("status")).toHaveTextContent(
      "Restored lost as a workspace. It is stopped; start it to use it.",
    );
    await vi.waitFor(() =>
      expect(document.activeElement).toBe(
        screen.getByRole("heading", {
          name: "A workspace disk is not in your workspace list",
        }),
      ),
    );
  });

  it("asks for the repository address when the disk gave none, then restores with it", async () => {
    const { api } = await mount(
      ["bare"],
      [
        {
          status: 422,
          error: {
            message: "Puddle could not read a repository address from ws-bare",
          },
        },
        { status: 201, data: { name: "bare" } },
      ],
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Restore as workspace ws-bare" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle could not read a repository address from ws-bare",
    );
    const field = screen.getByLabelText("Repository address for bare");
    await fireEvent.input(field, {
      target: { value: "https://example.org/acme/web" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Restore as workspace ws-bare" }),
    );
    await vi.waitFor(() => expect(api.POST).toHaveBeenCalledTimes(2));
    expect(api.POST).toHaveBeenLastCalledWith(
      "/api/orphan-volumes/{id}/restore",
      {
        params: { path: { id: "bare" } },
        body: { repository: "https://example.org/acme/web" },
      },
    );
    await vi.waitFor(() => expect(screen.queryByText("ws-bare")).toBeNull());
  });

  it("keeps the disk in the notice with the reason when a restore is refused", async () => {
    await mount(
      ["lost"],
      [
        {
          status: 409,
          error: {
            message:
              "a workspace named lost is already in the list; nothing was replaced",
          },
        },
      ],
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Restore as workspace ws-lost" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "nothing was replaced",
    );
    expect(screen.getByText("ws-lost")).toBeTruthy();
    expect(screen.queryByLabelText(/Repository address/)).toBeNull();
  });
});
