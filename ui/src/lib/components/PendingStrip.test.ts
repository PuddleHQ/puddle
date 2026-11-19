// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it } from "vitest";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { workspace as fakeWorkspace } from "#lib/testing/fake-workspaces.ts";
import { expectNoIdsShown } from "#lib/testing/no-ids.ts";
import PendingStrip from "./PendingStrip.svelte";

afterEach(cleanup);

describe("PendingStrip", () => {
  it("names the workspace asking, never its generated id", () => {
    workspaces.list = [{ ...fakeWorkspace("Shop"), id: "g07jxx5q" }];
    const { container } = render(PendingStrip, {
      props: {
        count: 1,
        latest: { workspace: "g07jxx5q", host: "cdn.example.org", port: 443 },
      },
    });
    expect(screen.getByRole("status")).toHaveTextContent("Shop wants");
    expectNoIdsShown(container, ["g07jxx5q"]);
    workspaces.list = [];
  });

  it("is not there when nothing waits", () => {
    const { container } = render(PendingStrip, { props: { count: 0 } });
    expect(container.textContent?.trim()).toBe("");
  });

  it("counts, says what is asking and links to the inbox", () => {
    render(PendingStrip, {
      props: {
        count: 3,
        latest: { workspace: "web-shop", host: "cdn.example.org", port: 443 },
      },
    });
    const strip = screen.getByRole("status");
    expect(strip).toHaveTextContent("3 requests waiting.");
    expect(strip).toHaveTextContent("web-shop wants cdn.example.org:443");
    expect(screen.getByRole("link", { name: "Review" })).toHaveAttribute(
      "href",
      "/inbox",
    );
  });

  it("says one request in the singular, and manages without a latest one", () => {
    render(PendingStrip, { props: { count: 1 } });
    expect(screen.getByRole("status")).toHaveTextContent("1 request waiting.");
    expect(screen.getByRole("status")).not.toHaveTextContent("Latest");
  });
});
