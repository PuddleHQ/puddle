// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { shellInitEvent } from "./shell-init.ts";

describe("shellInitEvent", () => {
  it("names the two events of one workspace", () => {
    expect(
      shellInitEvent(
        { type: "workspace_shell_init_changed", workspace: "web" },
        "web",
      ),
    ).toBe("changed");
    expect(
      shellInitEvent(
        { type: "workspace_shell_init_restored", workspace: "web" },
        "web",
      ),
    ).toBe("restored");
  });

  it("ignores another workspace, another event and what is not an event", () => {
    expect(
      shellInitEvent(
        { type: "workspace_shell_init_changed", workspace: "docs" },
        "web",
      ),
    ).toBeNull();
    expect(shellInitEvent({ type: "x", workspace: "web" }, "web")).toBeNull();
    expect(shellInitEvent(5, "web")).toBeNull();
    expect(shellInitEvent(null, "web")).toBeNull();
  });
});
