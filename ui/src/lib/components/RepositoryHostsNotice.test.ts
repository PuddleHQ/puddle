// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RepositoryHost } from "#lib/workspaces/hosts.ts";
import RepositoryHostsNotice from "./RepositoryHostsNotice.svelte";

afterEach(cleanup);

const host = (
  name: string,
  state: RepositoryHost["state"],
  blocked_by: string | null = null,
): RepositoryHost => ({ host: name, state, blocked_by });

function show(hosts: RepositoryHost[] | null, urls: string[]) {
  const check = vi.fn(async () => hosts);
  render(RepositoryHostsNotice, { props: { urls, check, delay: 0 } } as never);
  return check;
}

describe("RepositoryHostsNotice", () => {
  it("names the new hosts the create will allow", async () => {
    const check = show(
      [host("github.com", "new"), host("dev.azure.com", "allowed")],
      ["https://github.com/acme/web.git", "  "],
    );
    const box = await screen.findByTestId("hosts-new");
    expect(box).toHaveTextContent("allows github.com for this workspace");
    expect(check).toHaveBeenCalledWith(
      ["https://github.com/acme/web.git"],
      undefined,
    );
  });

  it("shows the blocking rule as an error", async () => {
    show(
      [
        host(
          "github.com",
          "blocked",
          "github.com is blocked by a deny rule (3).",
        ),
      ],
      ["https://github.com/acme/web.git"],
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "github.com is blocked by a deny rule (3).",
    );
  });

  it("says nothing without an address or when the service cannot answer", async () => {
    const none = show([host("github.com", "new")], ["", "ssh://x"]);
    await new Promise((r) => setTimeout(r, 20));
    expect(none).not.toHaveBeenCalled();
    cleanup();
    const down = show(null, ["https://github.com/acme/web.git"]);
    await vi.waitFor(() => expect(down).toHaveBeenCalled());
    expect(screen.queryByTestId("hosts-new")).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
