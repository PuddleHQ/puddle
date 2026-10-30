// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  editorInstallKey,
  editorInstallProblemOf,
  problemLink,
  startFolderKey,
  startFolderProblemOf,
  type Problem,
} from "./problem-links.ts";

const problem = (key: string, workspace: string | null): Problem => ({
  key,
  workspace,
  title: "t",
  detail: "d",
});

describe("where a problem sends you", () => {
  it("sends a missing start folder to the setting", () => {
    expect(problemLink(problem("start-folder:my web", "my web"))).toEqual({
      href: "/workspaces/my%20web/settings#start-folder",
      label: "Change start folder",
    });
  });

  it("sends any other workspace problem to the workspace, and one about nothing nowhere", () => {
    expect(problemLink(problem("git-locks:dev", "dev"))).toEqual({
      href: "/workspaces/dev",
      label: "Open workspace",
    });
    expect(problemLink(problem("clone:dev:api", "dev"))).toEqual({
      href: "/workspaces/dev",
      label: "Open workspace",
    });
    expect(problemLink(problem("sweeper", null))).toBeUndefined();
  });

  it("does not take another workspace's start folder for this one's", () => {
    const list = [problem(startFolderKey("other"), "other")];
    expect(startFolderProblemOf(list, "dev")).toBeUndefined();
    expect(startFolderProblemOf(list, "other")).toBe(list[0]);
  });

  it("finds the editor install problem of one workspace and no other", () => {
    const list = [
      problem(editorInstallKey("other"), "other"),
      problem(editorInstallKey("dev"), "dev"),
    ];
    expect(editorInstallProblemOf(list, "dev")).toBe(list[1]);
    expect(editorInstallProblemOf(list, "none")).toBeUndefined();
    expect(problemLink(list[1]!)).toEqual({
      href: "/workspaces/dev",
      label: "Open workspace",
    });
  });
});
