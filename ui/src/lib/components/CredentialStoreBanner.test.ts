// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CredentialStoreState } from "#lib/stores/credential-store.svelte.ts";
import CredentialStoreBanner from "./CredentialStoreBanner.svelte";

afterEach(cleanup);

const problem = {
  key: "credential-store",
  title: "Puddle cannot save secrets: the credential store is locked",
  detail: "Unlock it, then choose Retry in Puddle or restart Puddle.",
  workspace: null,
};
const other = { key: "sweep", title: "Other", detail: "x", workspace: null };

/** A host whose problem list is `standing` and whose retry answers `retry`. */
function host(standing: unknown[], retry: unknown) {
  const list = [...standing];
  return {
    list,
    api: {
      GET: vi.fn(async () => ({ data: { problems: list } })),
      POST: vi.fn(async () => {
        // A working retry clears the problem the way the host does.
        if ((retry as { data?: { working?: boolean } }).data?.working)
          list.length = 0;
        return retry;
      }),
    },
  };
}

async function mount(standing: unknown[], retry: unknown = {}) {
  const h = host(standing, retry);
  const store = new CredentialStoreState(h.api as never, undefined);
  await store.load();
  render(CredentialStoreBanner, { props: { store } } as never);
  return { ...h, store };
}

describe("the credential store banner on the first-run screen", () => {
  it("says nothing while the store works or other problems stand", async () => {
    await mount([other]);
    expect(screen.queryByRole("region")).toBeNull();
  });

  it("names the cause and the fix the host worded, with a Retry button", async () => {
    await mount([other, problem]);
    const banner = screen.getByRole("region", { name: problem.title });
    expect(banner).toHaveTextContent("Unlock it");
    expect(screen.getByRole("button", { name: "Retry" })).toBeEnabled();
  });

  it("retries without a restart and goes away when the host clears the problem", async () => {
    const { api } = await mount([problem], {
      data: { working: true },
      status: 200,
    });
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(api.POST).toHaveBeenCalledWith("/api/credential-store/retry");
    await vi.waitFor(() => expect(screen.queryByRole("region")).toBeNull());
  });

  it("stays and says so when the store still refuses", async () => {
    await mount([problem], { data: { working: false }, status: 200 });
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "still refuses writes",
    );
    expect(screen.getByRole("region")).toBeInTheDocument();
  });

  it("says why when Puddle refused the retry", async () => {
    await mount([problem], { error: { message: "Not available here." } });
    await fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Not available here.",
    );
  });
});
