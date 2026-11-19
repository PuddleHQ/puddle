// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { load as environment } from "../environment/+page.ts";
import { load as ports } from "../ports/+page.ts";
import { load as shellInit } from "../shell-init/+page.ts";
import { load as sshAgent } from "../ssh-agent/+page.ts";

/** Where a load function sends the visitor. */
function target(load: (event: never) => unknown, id: string): string {
  try {
    load({ params: { id } } as never);
  } catch (thrown) {
    return (thrown as { location: string }).location;
  }
  return "";
}

describe("the old addresses of the pages now under Settings", () => {
  it("send the visitor to the page under Settings, keeping the workspace", () => {
    expect(target(environment as never, "web shop")).toBe(
      "/workspaces/web%20shop/settings/environment",
    );
    expect(target(ports as never, "w")).toBe("/workspaces/w/settings/ports");
    expect(target(shellInit as never, "w")).toBe(
      "/workspaces/w/settings/shell-init",
    );
    expect(target(sshAgent as never, "w")).toBe(
      "/workspaces/w/settings/ssh-agent",
    );
  });
});
